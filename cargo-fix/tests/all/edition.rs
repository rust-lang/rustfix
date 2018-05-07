use super::project;

#[test]
#[ignore = "Requires custom rustc build"]
fn fix_edition_lints() {
    let p = project()
        .file(
            "Cargo.toml",
            r#"
                cargo-features = ["edition"]

                [package]
                name = "foo"
                version = "0.1.0"

                rust = "2018"

                [workspace]
            "#,
        )
        .file(
            "src/lib.rs",
            r#"
                #![allow(unused)]
                #[warn(rust_2018_migration)]

                mod private_mod {
                    pub const FOO: &str = "BAR";
                }

                fn main() {}

            "#,
        )
        .build();

    let stderr = "\
[CHECKING] foo v0.1.0 (CWD)
[FIXING] src/lib.rs (1 fix)
[FINISHED] dev [unoptimized + debuginfo]
";
    p.expect_cmd("cargo-fix fix")
        .stdout("")
        .stderr(stderr)
        .run();

    assert!(p.read("src/lib.rs").contains(r#"crate const FOO: &str = "BAR";"#));
}
