use super::*;
use serde_json::json;

fn fleet() -> Value {
    let owner = json!({"name":"host-a","kind":"standalone","platform":"nixos","build":{"system":"x86_64-linux"},"deployment":{"deployRs":{"enable":true}}});
    let guest = |name: &str| json!({"name":name,"kind":"vm","platform":"nixos","build":{"system":"x86_64-linux"},"vm":{"owner":"host-a","backend":"nixos-module"},"deployment":{"deployRs":{"enable":false},"ansible":{"enable":false}}});
    json!({"schemaVersion":2,"members":{"host-a":owner,"guest-a":guest("guest-a"),"guest-b":guest("guest-b")},"groups":{"guests":["guest-a","guest-b"]},"projections":{"vm":{"guestsByOwner":{"host-a":["guest-a","guest-b"]},"applications":{"host-a":{"owner":"host-a","backend":"nixos-module","manifestVersion":1,"supportedSystems":["x86_64-linux"],"guests":{"guest-a":{"system":"x86_64-linux"},"guest-b":{"system":"x86_64-linux"}},"application":".#deploy.nodes.host-a.profiles.system.path"}}}}})
}

#[test]
fn vm_owner_resolution_deduplicates_and_retains_sibling_impact() {
    let inv = Inventory::from_value(fleet()).unwrap();
    let plan = resolve(&inv, "guest-a").unwrap();
    assert_eq!(plan.selected_members, ["guest-a"]);
    assert_eq!(plan.requested_guests, ["guest-a"]);
    assert_eq!(plan.units.len(), 1);
    let unit = &plan.units["host-a"];
    assert_eq!(unit.affected_guests, ["guest-a", "guest-b"]);
    assert_eq!(unit.potentially_affected_guests, ["guest-b"]);
    assert!(unit.recovery_limit.contains("not reverted"));
    let all = resolve(&inv, "host-a,@guests").unwrap();
    assert_eq!(all.units.len(), 1);
    assert!(all.units["host-a"].potentially_affected_guests.is_empty());
}

#[test]
fn vm_owner_resolution_refuses_owner_and_sibling_exclusions() {
    let inv = Inventory::from_value(fleet()).unwrap();
    for selector in [
        "guest-a,!host-a",
        "guest-a,!guest-b",
        "@guests,!guest-b",
        "host-a,!guest-b",
    ] {
        assert!(
            resolve(&inv, selector)
                .unwrap_err()
                .to_string()
                .contains("conflicts"),
            "{selector}"
        );
    }
}

#[test]
fn vm_owner_resolution_approval_binds_revision_settings_and_impact() {
    let inv = Inventory::from_value(fleet()).unwrap();
    let plan = resolve(&inv, "guest-a").unwrap();
    let fingerprint = plan
        .fingerprint("revision-a", &json!({"sshUser":"operator-alice"}))
        .unwrap();
    assert_eq!(
        fingerprint,
        plan.fingerprint("revision-a", &json!({"sshUser":"operator-alice"}))
            .unwrap()
    );
    assert_ne!(
        fingerprint,
        plan.fingerprint("revision-b", &json!({"sshUser":"operator-alice"}))
            .unwrap()
    );
    assert_ne!(
        fingerprint,
        plan.fingerprint("revision-a", &json!({"sshUser":"root"}))
            .unwrap()
    );
    assert_ne!(
        fingerprint,
        resolve(&inv, "guest-b")
            .unwrap()
            .fingerprint("revision-a", &json!({"sshUser":"operator-alice"}))
            .unwrap()
    );
}

#[test]
fn vm_owner_resolution_rejects_forged_aggregate_before_effects() {
    for (pointer, value) in [
        ("/schemaVersion", json!(1)),
        ("/members/host-a/platform", json!("darwin")),
        ("/members/host-a/kind", json!("vm")),
        ("/members/guest-a/deployment/deployRs/enable", json!(true)),
        ("/members/guest-a/deployment/ansible/enable", json!(true)),
        ("/members/guest-a/vm/owner", json!("missing")),
        ("/members/guest-a/vm/backend", json!("guest-ssh")),
        ("/projections/vm/guestsByOwner/host-a", json!(["guest-a"])),
        (
            "/projections/vm/applications/host-a/manifestVersion",
            json!(2),
        ),
        (
            "/projections/vm/applications/host-a/application",
            json!(".#guest-a"),
        ),
        (
            "/projections/vm/applications/host-a/guests/guest-a/system",
            json!("aarch64-linux"),
        ),
    ] {
        let mut value_fleet = fleet();
        *value_fleet.pointer_mut(pointer).unwrap() = value;
        assert!(Inventory::from_value(value_fleet).is_err(), "{pointer}");
    }
}

#[test]
fn vm_owner_resolution_campaign_gate_is_closed_only_for_vm_units() {
    let inv = Inventory::from_value(fleet()).unwrap();
    assert!(
        ensure_campaign_supported(&inv, "guest-a")
            .unwrap_err()
            .to_string()
            .contains("frozen campaign")
    );
    assert!(ensure_campaign_supported(&inv, "host-a").is_err());
    let legacy = Inventory::from_value(
        json!({"schemaVersion":1,"members":{"node-a":{"name":"node-a"}},"groups":{}}),
    )
    .unwrap();
    assert!(ensure_campaign_supported(&legacy, "node-a").is_ok());
    assert_eq!(resolve(&legacy, "node-a").unwrap(), OwnerPlan::default());
}

#[test]
fn vm_owner_resolution_native_build_targets_only_deduplicated_owner() {
    let mut value = fleet();
    value["projections"]["deploy"] = json!({"settings":{"host-a":{
        "activation":"switch", "hostname":"owner.example.test", "sshUser":"operator-alice",
        "sshPort":2222, "autoRollback":true, "fastConnection":false
    }}});
    let inv = Inventory::from_value(value).unwrap();
    let plan = crate::engines::deploy::plan_run(&inv, "host-a,@guests", 4, false).unwrap();
    assert_eq!(plan.targets, ["host-a"]);
    assert!(plan.skipped.is_empty());
    assert_eq!(
        plan.preview()["ownerPlan"]["requestedGuests"],
        json!(["guest-a", "guest-b"])
    );
    let argv = crate::engines::deploy::build_run_argv(
        "path:/frozen",
        &plan.targets,
        std::path::Path::new("/tmp/result"),
    );
    assert_eq!(argv.iter().filter(|s| *s == "build").count(), 1);
    assert_eq!(
        argv.iter()
            .filter(|s| s.contains("#deploy.nodes."))
            .collect::<Vec<_>>(),
        ["path:/frozen#deploy.nodes.host-a.profiles.system.path"]
    );
    assert!(matches!(
        crate::engines::deploy::register_run(plan),
        Err(crate::engines::deploy::DeployRunError::OwnerBackendUnavailable)
    ));
}

fn path(name: &str) -> String {
    format!("/nix/store/00000000000000000000000000000000-{name}")
}

fn manifest() -> (manifest::Manifest, OwnerUnit, BTreeSet<String>) {
    let plan = resolve(&Inventory::from_value(fleet()).unwrap(), "guest-a").unwrap();
    let unit = plan.units["host-a"].clone();
    let guests = unit
        .affected_guests
        .iter()
        .map(|name| {
            (
                name.clone(),
                manifest::Guest {
                    system: "x86_64-linux".into(),
                    desired_identity: format!("image:{name}:new"),
                    artifact: path(name),
                    runtime_instance: format!("runtime-{name}"),
                    readiness: manifest::Readiness {
                        contract: path("probe"),
                        deadline_seconds: 30,
                    },
                    recovery_contract: path("restore"),
                },
            )
        })
        .collect();
    let manifest = manifest::Manifest {
        manifest_version: 1,
        owner: "host-a".into(),
        backend: "nixos-module".into(),
        owner_system: "x86_64-linux".into(),
        supported_systems: vec!["x86_64-linux".into()],
        adapter: format!("{}/bin/adapter", path("adapter")),
        guests,
    };
    let closure = ["adapter", "guest-a", "guest-b", "probe", "restore"]
        .into_iter()
        .map(path)
        .collect();
    (manifest, unit, closure)
}

#[test]
fn vm_owner_resolution_manifest_requires_complete_immutable_closure() {
    let (m, unit, mut closure) = manifest();
    assert!(m.validate(&unit, &closure).is_ok());
    closure.remove(&path("guest-b"));
    assert!(m.validate(&unit, &closure).is_err());
    for bad in [
        "/tmp/image",
        "/nix/store/00000000000000000000000000000000-image/../escape",
    ] {
        let mut m = m.clone();
        m.guests.get_mut("guest-a").unwrap().artifact = bad.into();
        assert!(m.validate(&unit, &closure).is_err());
    }
}

#[test]
fn vm_owner_resolution_manifest_bounds_readiness_and_matches_preview() {
    let (m, unit, closure) = manifest();
    for deadline in [0, 3601, u64::MAX] {
        let mut m = m.clone();
        m.guests
            .get_mut("guest-a")
            .unwrap()
            .readiness
            .deadline_seconds = deadline;
        assert!(m.validate(&unit, &closure).is_err());
    }
    let mut m = m;
    m.guests.remove("guest-b");
    assert!(m.validate(&unit, &closure).is_err());
}

#[test]
fn vm_owner_resolution_prior_runtime_required_for_every_sibling() {
    let (m, _, _) = manifest();
    let mut previous = manifest::Observation {
        owner_profile: path("owner-profile"),
        owner_running: path("owner-running"),
        boot_default: path("boot-default"),
        guests: m
            .guests
            .iter()
            .map(|(name, guest)| {
                (
                    name.clone(),
                    manifest::Runtime {
                        running_identity: format!("image:{name}:old"),
                        runtime_instance: guest.runtime_instance.clone(),
                        artifact: path(&format!("{name}-old")),
                        ready: true,
                        diagnostic: String::new(),
                    },
                )
            })
            .collect(),
    };
    assert!(m.validate_previous(&previous).is_ok());
    previous
        .guests
        .get_mut("guest-a")
        .unwrap()
        .running_identity
        .clear();
    assert!(m.validate_previous(&previous).is_err());
    previous.guests.remove("guest-a");
    assert!(m.validate_previous(&previous).is_err());
}
