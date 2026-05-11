use silabs_data_gen::pdsc;

#[test]
fn parses_mg26_fixture() {
    let xml = include_str!("fixtures/mg26_min.pdsc");
    let db = pdsc::parse(xml).expect("parses");
    assert_eq!(db.family, "EFR32MG26");
    assert!(db.chips.len() >= 2, "expected >= 2 chips, got {}", db.chips.len());

    let first = &db.chips[0];
    assert!(first.name.starts_with("EFR32MG26"), "got {}", first.name);
    assert_eq!(first.core, "Cortex-M33");
    // The real MG26 pdsc declares NO_FPU / NO_MPU / NO_TZ at family level —
    // the parser must correctly interpret those negative tokens.
    assert!(!first.fpu, "Dfpu=NO_FPU must yield fpu=false");
    assert!(!first.mpu, "Dmpu=NO_MPU must yield mpu=false");
    assert!(!first.trustzone, "Dtz=NO_TZ must yield trustzone=false");
    assert!(
        first.memory.iter().any(|m| m.id == "IROM1"),
        "memory: {:?}",
        first.memory
    );
    assert!(
        first.memory.iter().any(|m| m.id == "IRAM1"),
        "memory: {:?}",
        first.memory
    );
    assert!(first.svd.ends_with(".svd"));

    // Check hex parsing: IROM1 starts at 0x08000000, IRAM1 at 0x20000000.
    let irom = first.memory.iter().find(|m| m.id == "IROM1").unwrap();
    assert_eq!(irom.start, 0x0800_0000);
    assert_eq!(irom.size, 0x0020_0000);
    let iram = first.memory.iter().find(|m| m.id == "IRAM1").unwrap();
    assert_eq!(iram.start, 0x2000_0000);

    // Default access conventions.
    assert_eq!(irom.access, "rx");
    assert_eq!(iram.access, "rwx");

    // Flash algo + SVD path round-trip.
    assert_eq!(first.flash_algo.as_deref(), Some("Flash/GECKOS2C3.FLM"));
    assert!(first.svd.contains("EFR32MG26"));
}
