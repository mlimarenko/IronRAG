#[test]
fn image_checksum_sha256_hex_format() {
    use hex;
    use sha2::{Digest, Sha256};
    let bytes: &[&[u8]] = &[b"image1", b"image2"];
    let mut sorted = bytes.to_vec();
    sorted.sort();
    let mut hasher = Sha256::new();
    for b in &sorted {
        hasher.update(b);
    }
    let result = hex::encode(hasher.finalize());
    assert_eq!(result.len(), 64);
    assert!(result.chars().all(|c| c.is_ascii_hexdigit()));
    let mut hasher2 = Sha256::new();
    for b in &sorted {
        hasher2.update(b);
    }
    let result2 = hex::encode(hasher2.finalize());
    assert_eq!(result, result2);
}

#[test]
fn image_checksum_sort_is_deterministic() {
    use hex;
    use sha2::{Digest, Sha256};
    let order_a: &[&[u8]] = &[b"alpha", b"beta"];
    let order_b: &[&[u8]] = &[b"beta", b"alpha"];
    let hash = |slices: &[&[u8]]| {
        let mut v = slices.to_vec();
        v.sort();
        let mut h = Sha256::new();
        for b in &v {
            h.update(b);
        }
        hex::encode(h.finalize())
    };
    assert_eq!(hash(order_a), hash(order_b));
}
