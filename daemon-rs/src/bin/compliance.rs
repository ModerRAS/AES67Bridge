//! aes67-compliance: runs the same checks as `tests/compliance.rs` and prints
//! the PASS/FAIL matrix for the report or CI.

fn main() {
    let checks = aes67d::compliance::run_all();
    let mut failures = 0;
    println!(
        "{:<32} {:<5} {}",
        "AES67 CHECK", "STATE", "EVIDENCE"
    );
    for check in &checks {
        if !check.pass {
            failures += 1;
        }
        println!(
            "{:<32} {:<5} {}",
            check.id,
            if check.pass { "PASS" } else { "FAIL" },
            check.detail
        );
    }
    println!(
        "\n{} checks, {} passed, {} failed",
        checks.len(),
        checks.len() - failures,
        failures
    );
    if failures != 0 {
        std::process::exit(1);
    }
}
