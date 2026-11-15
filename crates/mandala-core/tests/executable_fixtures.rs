//! Regression coverage for executable fixture publication under parallel spawn.
mod executable {
    include!("support/executable.rs");
}

use executable::write_program;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::process::Command;

#[test]
fn replacement_publishes_a_new_closed_executable_inode() {
    let directory =
        std::env::temp_dir().join(format!("mandala-fixture-replace-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("program");
    write_program(&path, "#!/bin/sh\necho old\n").unwrap();
    let mut old = std::fs::File::open(&path).unwrap();
    write_program(&path, "#!/bin/sh\necho new\n").unwrap();
    assert_ne!(
        old.metadata().unwrap().ino(),
        path.metadata().unwrap().ino()
    );
    let mut text = String::new();
    old.read_to_string(&mut text).unwrap();
    assert_eq!(text, "#!/bin/sh\necho old\n");
    let output = Command::new(&path).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"new\n");
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn parallel_writers_and_spawns_do_not_leak_writable_executable_fds() {
    let directory =
        std::env::temp_dir().join(format!("mandala-fixture-parallel-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::thread::scope(|scope| {
        for index in 0..16 {
            let path = directory.join(format!("program-{index}"));
            scope.spawn(move || {
                for generation in 0..16 {
                    write_program(&path, format!("#!/bin/sh\necho {generation}\n")).unwrap();
                    let output = Command::new(&path).output().unwrap();
                    assert!(output.status.success());
                    assert_eq!(output.stdout, format!("{generation}\n").as_bytes());
                }
            });
        }
    });
    std::fs::remove_dir_all(directory).unwrap();
}
