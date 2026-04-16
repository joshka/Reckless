#[repr(C)]
/// Represents the sets of random numbers used to produce an *almost* unique hash value
/// for a position using [Zobrist Hashing](https://en.wikipedia.org/wiki/Zobrist_hashing)
/// generated using the SplitMix64 pseudorandom number generator.
pub struct Zobrist {
    pub pieces: [[u64; 64]; 12],
    pub en_passant: [u64; 64],
    pub castling: [u64; 16],
    pub halfmove_clock: [u64; 16],
    pub side: u64,
}

const fn splitmix64_sequence() -> [u64; 865] {
    const SEED: u64 = 0xFFAA_B58C_5833_FE89u64;
    const INCREMENT: u64 = 0x9E37_79B9_7F4A_7C15;

    let mut zobrist = [0; 865];
    let mut state = SEED;
    let mut i = 0;

    while i < zobrist.len() {
        state = state.wrapping_add(INCREMENT);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        zobrist[i] = z ^ (z >> 31);
        i += 1;
    }

    zobrist
}

const fn build_zobrist(words: [u64; 865]) -> Zobrist {
    let mut index = 0;

    let mut pieces = [[0; 64]; 12];
    let mut piece = 0;
    while piece < pieces.len() {
        let mut square = 0;
        while square < pieces[piece].len() {
            pieces[piece][square] = words[index];
            index += 1;
            square += 1;
        }
        piece += 1;
    }

    let mut en_passant = [0; 64];
    let mut square = 0;
    while square < en_passant.len() {
        en_passant[square] = words[index];
        index += 1;
        square += 1;
    }

    let mut castling = [0; 16];
    let mut right = 0;
    while right < castling.len() {
        castling[right] = words[index];
        index += 1;
        right += 1;
    }

    let mut halfmove_clock = [0; 16];
    let mut ply = 0;
    while ply < halfmove_clock.len() {
        halfmove_clock[ply] = words[index];
        index += 1;
        ply += 1;
    }

    let side = words[index];

    Zobrist { pieces, en_passant, castling, halfmove_clock, side }
}

pub const ZOBRIST: Zobrist = build_zobrist(splitmix64_sequence());
