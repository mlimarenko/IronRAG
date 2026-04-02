use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let migrations_dir = Path::new("migrations");
    let mut migration_files = Vec::new();

    collect_sql_files(migrations_dir, &mut migration_files);
    migration_files.sort();

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", migrations_dir.display());

    let mut hasher = Fnv64::default();
    for path in &migration_files {
        println!("cargo:rerun-if-changed={}", path.display());
        let contents = fs::read(path).expect("failed to read migration file for fingerprint");
        hasher.update(path.display().to_string().as_bytes());
        hasher.update(&contents.len().to_le_bytes());
        hasher.update(&contents);
    }

    println!("cargo:rustc-env=RUSTRAG_MIGRATIONS_FINGERPRINT={:016x}", hasher.finish());
}

fn collect_sql_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("failed to read migrations directory") {
        let entry = entry.expect("failed to read migrations directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_sql_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "sql") {
            files.push(path);
        }
    }
}

#[derive(Default)]
struct Fnv64(u64);

impl Fnv64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn update(&mut self, bytes: &[u8]) {
        if self.0 == 0 {
            self.0 = Self::OFFSET_BASIS;
        }
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    fn finish(self) -> u64 {
        if self.0 == 0 { Self::OFFSET_BASIS } else { self.0 }
    }
}
