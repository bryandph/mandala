use mandala_core::runner::{ansible_dir, reboot_argv};

#[test]
fn reboot_uses_the_canonical_flake_app_not_path_or_a_raw_playbook() {
    let root = std::env::temp_dir().join(format!("mandala-flake-paths-{}", std::process::id()));
    std::fs::create_dir_all(root.join("ansible")).unwrap();
    std::fs::write(root.join("ansible/ansible.cfg"), "[defaults]\n").unwrap();
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let canonical = root.canonicalize().unwrap();
    assert_eq!(ansible_dir(&alias), canonical.join("ansible"));
    assert_eq!(
        reboot_argv(&alias, "web", "25%", true).unwrap(),
        [
            "nix",
            "run",
            &format!("{}#ans-reboot", canonical.display()),
            "--",
            "-l",
            "web",
            "-e",
            "reboot_serial=25%",
            "-e",
            "drain=true"
        ]
    );
    std::fs::remove_dir_all(root).unwrap();
}
