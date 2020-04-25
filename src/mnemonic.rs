use rand::seq::SliceRandom;
use rand::thread_rng;

static WORDS_DAT: &str = include_str!("../dat/nounlist.txt");

lazy_static! {
    static ref WORD_LIST: Vec<&'static str> = WORDS_DAT.lines().collect();
}

pub fn random_word() -> &'static str {
    WORD_LIST.as_slice().choose(&mut thread_rng()).unwrap()
}
