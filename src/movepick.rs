use crate::{
    lookup::king_attacks,
    search::NodeType,
    setwise::{bishop_attacks_setwise, knight_attacks_setwise, pawn_attacks_setwise, rook_attacks_setwise},
    thread::ThreadData,
    types::{ArrayVec, Bitboard, MAX_MOVES, Move, MoveEntry, MoveList, PieceType},
};

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd)]
pub enum Stage {
    HashMove,
    GenerateNoisy,
    GoodNoisy,
    GenerateQuiet,
    Quiet,
    BadNoisy,
}

pub struct MovePicker {
    list: MoveList,
    tt_move: Move,
    threshold: Option<i32>,
    stage: Stage,
    bad_noisy: ArrayVec<Move, MAX_MOVES>,
    bad_noisy_idx: usize,
}

impl MovePicker {
    pub const fn new(tt_move: Move) -> Self {
        Self {
            list: MoveList::new(),
            tt_move,
            threshold: None,
            stage: if tt_move.is_present() { Stage::HashMove } else { Stage::GenerateNoisy },
            bad_noisy: ArrayVec::new(),
            bad_noisy_idx: 0,
        }
    }

    pub const fn new_probcut(threshold: i32) -> Self {
        Self {
            list: MoveList::new(),
            tt_move: Move::NULL,
            threshold: Some(threshold),
            stage: Stage::GenerateNoisy,
            bad_noisy: ArrayVec::new(),
            bad_noisy_idx: 0,
        }
    }

    pub const fn new_qsearch() -> Self {
        Self {
            list: MoveList::new(),
            tt_move: Move::NULL,
            threshold: None,
            stage: Stage::GenerateNoisy,
            bad_noisy: ArrayVec::new(),
            bad_noisy_idx: 0,
        }
    }

    pub const fn stage(&self) -> Stage {
        self.stage
    }

    pub fn next<NODE: NodeType>(&mut self, td: &ThreadData, skip_quiets: bool, ply: isize) -> Option<Move> {
        if self.stage == Stage::HashMove {
            self.stage = Stage::GenerateNoisy;

            if td.board.is_legal(self.tt_move) {
                return Some(self.tt_move);
            }
        }

        if self.stage == Stage::GenerateNoisy {
            self.stage = Stage::GoodNoisy;
            td.board.append_noisy_moves(&mut self.list);
            self.score_noisy(td);
        }

        if self.stage == Stage::GoodNoisy {
            while !self.list.is_empty() {
                let entry = self.get_best_entry();
                if entry.mv == self.tt_move {
                    continue;
                }

                let threshold = self.threshold.unwrap_or_else(|| -entry.score / 45 + 111);
                if !td.board.see(entry.mv, threshold) {
                    self.bad_noisy.push(entry.mv);
                    continue;
                }

                if NODE::ROOT {
                    self.score_noisy(td);
                }

                return Some(entry.mv);
            }

            if skip_quiets {
                self.stage = Stage::BadNoisy;
            } else {
                self.stage = Stage::GenerateQuiet;
            }
        }

        if self.stage == Stage::GenerateQuiet {
            self.stage = Stage::Quiet;
            td.board.append_quiet_moves(&mut self.list);
            self.score_quiet(td, ply);
        }

        if self.stage == Stage::Quiet {
            if !skip_quiets {
                while !self.list.is_empty() {
                    let entry = self.get_best_entry();
                    if entry.mv == self.tt_move {
                        continue;
                    }

                    if NODE::ROOT {
                        self.score_quiet(td, ply);
                    }

                    return Some(entry.mv);
                }
            }

            self.stage = Stage::BadNoisy;
        }

        // Stage::BadNoisy
        if self.bad_noisy_idx < self.bad_noisy.len() {
            let mv = self.bad_noisy[self.bad_noisy_idx];
            self.bad_noisy_idx += 1;
            return Some(mv);
        }

        None
    }

    fn get_best_entry(&mut self) -> MoveEntry {
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            // On AVX2-capable x86 targets, scan scores eight at a time instead of
            // comparing one entry per iteration. This only changes how we find
            // the best-scoring move; it preserves the scalar path's behavior,
            // including preferring the later entry on equal scores.
            let best_index = unsafe { best_index_avx2(self.list.iter()) };
            self.list.remove(best_index)
        }

        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
        {
            let mut best_index = 0;
            let mut best_score = i32::MIN;

            for (index, entry) in self.list.iter().enumerate() {
                if entry.score >= best_score {
                    best_index = index;
                    best_score = entry.score;
                }
            }

            self.list.remove(best_index)
        }
    }

    fn score_noisy(&mut self, td: &ThreadData) {
        let threats = td.board.all_threats();

        for entry in self.list.iter_mut() {
            let mv = entry.mv;
            let captured = td.board.type_on(mv.capture_sq());
            let pt = td.board.type_on(mv.from());

            entry.score = 16 * captured.value()
                + td.noisy_history.get(threats, td.board.moved_piece(mv), mv.to(), captured)
                + 4000 * (mv.is_promotion() && mv.promo_piece_type() == PieceType::Queen) as i32
                + (200000 - 20000 * pt as i32) * td.board.in_check() as i32;
        }
    }

    fn score_quiet(&mut self, td: &ThreadData, ply: isize) {
        let threats = td.board.all_threats();
        let side = td.board.side_to_move();
        let occupancies = td.board.occupancies();

        let threatened = {
            let pawn_threats = td.board.piece_threats(PieceType::Pawn);
            let minor_threats =
                pawn_threats | td.board.piece_threats(PieceType::Knight) | td.board.piece_threats(PieceType::Bishop);
            let rook_threats = minor_threats | td.board.piece_threats(PieceType::Rook);
            [Bitboard(0), pawn_threats, pawn_threats, minor_threats, rook_threats, Bitboard(0)]
        };

        let escape = [0, 7768, 8218, 13424, 20208, 0];

        // safe squares where we can attack an opponent piece
        let offense = {
            let knight_vulnerable = (td.board.colored_pieces(!side, PieceType::Bishop) & !threats)
                | td.board.colored_pieces(!side, PieceType::Rook)
                | td.board.colored_pieces(!side, PieceType::Queen);
            let bishop_vulnerable = td.board.colored_pieces(!side, PieceType::Rook);
            let queen_orth_vulnerable = td.board.colored_pieces(!side, PieceType::Bishop) & !threats;
            let queen_diag_vulnerable = td.board.colored_pieces(!side, PieceType::Rook) & !threats;

            let p = pawn_attacks_setwise(td.board.colors(!side), !side);
            let n = knight_attacks_setwise(knight_vulnerable);
            let b = bishop_attacks_setwise(bishop_vulnerable, occupancies);
            let q = rook_attacks_setwise(queen_orth_vulnerable, occupancies)
                | bishop_attacks_setwise(queen_diag_vulnerable, occupancies);

            [p & !threats, n & !threats, b & !threats, Bitboard(0), q & !threats, Bitboard(0)]
        };

        let king_file = td.board.king_square(!side).file();

        // don't move king wall pawns
        let wall_pawns = if Bitboard::HOME_ROWS[side].contains(td.board.king_square(side)) {
            king_attacks(td.board.king_square(side)) & td.board.pieces(PieceType::Pawn)
        } else {
            Bitboard(0)
        };

        for entry in self.list.iter_mut() {
            let mv = entry.mv;
            let pt = td.board.type_on(mv.from());

            entry.score = 2048 * td.quiet_history.get(threats, side, mv) / 1024
                + 1536 * td.conthist(ply, 1, mv) / 1024
                + td.conthist(ply, 2, mv)
                + td.conthist(ply, 4, mv)
                + td.conthist(ply, 6, mv)
                + escape[pt] * threatened[pt].contains(mv.from()) as i32
                + 9325 * td.board.checking_squares(pt).contains(mv.to()) as i32
                - 7584 * threatened[pt].contains(mv.to()) as i32
                + 6158 * offense[pt].contains(mv.to()) as i32
                + 5000 * (pt == PieceType::Rook && king_file == mv.to().file()) as i32
                - 4000 * wall_pawns.contains(mv.from()) as i32;
        }
    }
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
unsafe fn best_index_avx2(entries: std::slice::Iter<'_, MoveEntry>) -> usize {
    use std::arch::x86_64::*;

    const LANES: usize = 8;

    let len = entries.len();
    let ptr = entries.as_slice().as_ptr().cast::<i32>().add(1);

    let mut best_scores = _mm256_set1_epi32(i32::MIN);
    let mut best_indices = _mm256_set1_epi32(-1);

    // MoveEntry stores the move and score together, so AVX2 gathers let us
    // load just the score lane from eight entries without changing the data
    // layout. That makes this a targeted experiment in speeding up move
    // selection while leaving generation and scoring unchanged.
    for base in (0..len / LANES * LANES).step_by(LANES) {
        let indices = _mm256_setr_epi32(
            base as i32,
            base as i32 + 1,
            base as i32 + 2,
            base as i32 + 3,
            base as i32 + 4,
            base as i32 + 5,
            base as i32 + 6,
            base as i32 + 7,
        );
        let offsets = _mm256_add_epi32(indices, indices);
        let scores = _mm256_i32gather_epi32(ptr, offsets, 4);

        let better_scores = _mm256_cmpgt_epi32(scores, best_scores);
        let equal_scores = _mm256_cmpeq_epi32(scores, best_scores);
        let later_indices = _mm256_cmpgt_epi32(indices, best_indices);
        let replace = _mm256_or_si256(better_scores, _mm256_and_si256(equal_scores, later_indices));

        best_scores = _mm256_blendv_epi8(best_scores, scores, replace);
        best_indices = _mm256_blendv_epi8(best_indices, indices, replace);
    }

    let mut score_buffer = [0; LANES];
    let mut index_buffer = [0; LANES];
    _mm256_storeu_si256(score_buffer.as_mut_ptr().cast(), best_scores);
    _mm256_storeu_si256(index_buffer.as_mut_ptr().cast(), best_indices);

    let mut best_index = 0;
    let mut best_score = i32::MIN;

    for (&score, &index) in score_buffer.iter().zip(index_buffer.iter()) {
        if score > best_score || (score == best_score && index > best_index as i32) {
            best_score = score;
            best_index = index as usize;
        }
    }

    for (index, entry) in entries.as_slice().iter().enumerate().skip(len / LANES * LANES) {
        if entry.score > best_score || (entry.score == best_score && index > best_index) {
            best_score = entry.score;
            best_index = index;
        }
    }

    best_index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MoveKind, Square};

    #[test]
    fn get_best_entry_prefers_later_equal_scores() {
        let mut picker = MovePicker::new(Move::NULL);

        for index in 0..10 {
            picker.list.push(Square::new(index as u8), Square::new((index + 1) as u8), MoveKind::Normal);
        }

        let scores = [10, 50, 5, 50, 40, 50, -1, 30, 50, 12];
        for (entry, score) in picker.list.iter_mut().zip(scores) {
            entry.score = score;
        }

        let best = picker.get_best_entry();

        assert_eq!(best.mv.from(), Square::new(8));
        assert_eq!(best.score, 50);
        assert_eq!(picker.list.len(), 9);
    }
}
