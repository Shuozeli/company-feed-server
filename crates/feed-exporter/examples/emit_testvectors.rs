// Emits the canonical selector test vectors as JSON. Run:
//   cargo run -p feed-exporter --example emit_testvectors
fn main() {
    let v = feed_exporter::canonical_test_vectors();
    let doc = serde_json::json!({
        "algorithm": "bucket = u32_be(sha256(salt || 0x00 || key)[0..4]) % bucket_count",
        "note": "Rust and JS (company-news-ui) MUST both reproduce these exactly.",
        "vectors": v,
    });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap());
}
