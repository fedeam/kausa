//! The license texts in LICENSES/ (the REUSE layout, which the OpenSSF Best Practices badge looks
//! for) are copies of LICENSE-MIT and LICENSE-APACHE: this keeps them the same.

#[test]
fn the_licenses_directory_matches_the_license_files() {
    assert_eq!(
        include_bytes!("../LICENSE-MIT").as_slice(),
        include_bytes!("../LICENSES/MIT.txt").as_slice(),
        "LICENSES/MIT.txt differs from LICENSE-MIT"
    );
    assert_eq!(
        include_bytes!("../LICENSE-APACHE").as_slice(),
        include_bytes!("../LICENSES/Apache-2.0.txt").as_slice(),
        "LICENSES/Apache-2.0.txt differs from LICENSE-APACHE"
    );
}
