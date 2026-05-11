use silabs_data_gen::svd;

#[test]
fn ns_and_s_have_identical_fingerprints() {
    let xml = include_str!("fixtures/eusart0_min.svd");
    let ps = svd::parse(xml).unwrap();
    let ns = ps.iter().find(|p| p.name == "EUSART0_NS").expect("NS missing");
    let s = ps.iter().find(|p| p.name == "EUSART0_S").expect("S missing");
    assert_eq!(ns.fingerprint, s.fingerprint, "NS/S register layouts must match");
    assert_ne!(ns.base_address, s.base_address);
    assert_eq!(ns.registers.len(), 2);
}
