//! AES67 compliance matrix as executable tests. The checks themselves live in
//! `aes67d::compliance`, shared with the `aes67-compliance` binary.

use aes67d::compliance::run_all;

#[test]
fn aes67_compliance_matrix_passes() {
    let checks = run_all();
    let mut failures = Vec::new();
    for check in &checks {
        println!("{:<32} {} {}", check.id, if check.pass { "PASS" } else { "FAIL" }, check.detail);
        if !check.pass {
            failures.push(check.id);
        }
    }
    assert!(failures.is_empty(), "failed checks: {failures:?}");
}
