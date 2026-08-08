//! Deterministic hostile-image campaign for the mmap reader boundary.
//!
//! The short target is a normal test. The ignored target accepts the same
//! shard/iteration environment variables as CI's long campaigns, so a failure
//! is reproducible without saving attacker-controlled files in the repository.

use dictc::{compile, parse_connection, parse_entries};
use sakura_core::dictionary::Dictionary;
use sakura_proto::{FixedStr, MAX_PREEDIT_BYTES};

const DEFAULT_ITERATIONS: u64 = 10_000;

#[test]
fn hostile_dictionary_images_never_crash_or_escape_bounds() {
    campaign(DEFAULT_ITERATIONS, 0, 0);
}

#[test]
#[ignore = "long deterministic campaign; set SAKURA_FUZZ_ITERS and SAKURA_FUZZ_SHARD"]
fn sharded_hostile_dictionary_campaign() {
    let iterations = env_u64("SAKURA_FUZZ_ITERS").unwrap_or(2_000_000);
    let shard = env_u64("SAKURA_FUZZ_SHARD").unwrap_or(0);
    let slice_seed = env_u64("SAKURA_FUZZ_SEED").unwrap_or(0);
    campaign(iterations, shard, slice_seed);
}

fn campaign(iterations: u64, shard: u64, slice_seed: u64) {
    let valid = fixture();
    let mut random =
        Random::new(0xd1c7_10a9_5eed_0001 ^ shard.rotate_left(17) ^ slice_seed.rotate_left(31));
    for iteration in 0..iterations {
        let seed = random.state;
        let mut image = match iteration % 5 {
            0 => random_bytes(&mut random, 4_096),
            _ => valid.clone(),
        };
        mutate(&mut image, &mut random, iteration);
        let outcome = std::panic::catch_unwind(|| exercise(&image));
        assert!(
            outcome.is_ok(),
            "dictionary reader panicked at shard {shard}, slice seed {slice_seed}, iteration {iteration}, PRNG seed {seed:#018x}, bytes {}",
            image.len()
        );
    }
}

fn fixture() -> Vec<u8> {
    let entries = parse_entries(
        "robustness.tsv",
        "# license: MIT\n\
         reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
         かな\t仮名\t1\t1\t100\t200\tpredict\twriting system\n\
         かんすう\t関数\t1\t1\t200\t300\tit,predict\tprogramming\n",
    )
    .expect("fixture entries");
    let matrix = parse_connection(
        "robustness-matrix.tsv",
        "# license: MIT\nclasses\t3\ndefault\t7\n",
        false,
    )
    .expect("fixture matrix");
    compile(&entries, &matrix).expect("fixture image")
}

fn random_bytes(random: &mut Random, maximum: usize) -> Vec<u8> {
    let len = random.usize(maximum + 1);
    (0..len).map(|_| random.next() as u8).collect()
}

fn mutate(image: &mut Vec<u8>, random: &mut Random, iteration: u64) {
    match iteration % 5 {
        0 => {}
        1 => image.truncate(random.usize(image.len() + 1)),
        2 => {
            let changes = 1 + random.usize(8);
            for _ in 0..changes {
                if image.is_empty() {
                    break;
                }
                let at = random.usize(image.len());
                image[at] ^= 1u8 << random.usize(8);
            }
        }
        3 => {
            if image.len() >= 4 {
                let at = random.usize(image.len() - 3);
                image[at..at + 4].copy_from_slice(&(random.next() as u32).to_le_bytes());
            }
        }
        _ => {
            let extra = random.usize(64);
            image.extend((0..extra).map(|_| random.next() as u8));
        }
    }
}

fn exercise(image: &[u8]) {
    let Ok(dictionary) = Dictionary::parse(image) else {
        return;
    };
    for reading in ["", "かな", "かんすう", "🚀かな", "a\0b", "\u{10ffff}"] {
        let _ = dictionary.common_prefix_search(reading, |matched| {
            let mut surface = FixedStr::<MAX_PREEDIT_BYTES>::new();
            let mut annotation = FixedStr::<MAX_PREEDIT_BYTES>::new();
            let _ = dictionary.write_surface(matched.entry, &mut surface);
            let _ = dictionary.write_annotation(matched.entry, &mut annotation);
            true
        });
    }
    for id in [0, 1, u16::MAX] {
        let _ = dictionary.connection_cost(id, id);
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

struct Random {
    state: u64,
}

impl Random {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn usize(&mut self, exclusive_end: usize) -> usize {
        if exclusive_end == 0 {
            0
        } else {
            (self.next() as usize) % exclusive_end
        }
    }
}
