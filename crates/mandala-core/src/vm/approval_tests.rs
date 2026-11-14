use super::*;
use crate::engines::deploy;
use crate::registry;
use serde_json::json;

#[test]
fn vm_owner_backend_approval_refuses_missing_stale_changed_and_disappeared_impact_before_registry()
{
    let value: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/vm-inventory.json")).unwrap();
    let inv = Inventory::from_value(value.clone()).unwrap();
    let plan = deploy::plan_run(&inv, "guest-a", 4, false).unwrap();
    let root = std::env::temp_dir().join(format!("mandala-owner-approval-{}", std::process::id()));
    let _runs = registry::test_hooks::install_runs_base(root.join("runs"));
    let token = plan.required_owner_approval().unwrap();
    assert_eq!(plan.preview()["requiredOwnerApproval"], token);
    assert!(deploy::register_run(plan.clone()).is_err());
    assert!(deploy::register_approved_run(plan.clone(), Some("stale")).is_err());
    let mut variants = Vec::new();
    let mut changed = plan.clone();
    changed.vm_revision = Some("new-inventory-content".into());
    variants.push(changed);
    let mut changed = plan.clone();
    changed.settings.get_mut("host-a").unwrap()["sshUser"] = json!("operator-bob");
    variants.push(changed);
    let mut changed = plan.clone();
    changed.boot = true;
    variants.push(changed);
    let mut changed = plan.clone();
    changed.dry_activate = true;
    variants.push(changed);
    let mut changed = plan.clone();
    let unit = changed.vm.units.get_mut("host-a").unwrap();
    unit.application
        .guests
        .insert("guest-c".into(), unit.application.guests["guest-b"].clone());
    unit.affected_guests.push("guest-c".into());
    unit.potentially_affected_guests.push("guest-c".into());
    variants.push(changed);
    let mut changed = plan.clone();
    changed.vm = OwnerPlan::default();
    variants.push(changed);
    for changed in variants {
        assert_ne!(changed.required_owner_approval(), Some(token.clone()));
        assert!(deploy::register_approved_run(changed, Some(&token)).is_err());
    }
    let mut changed = value;
    changed["members"]["host-a"]["tags"] = json!(["revision-b"]);
    let fresh = deploy::plan_run(
        &Inventory::from_value(changed).unwrap(),
        "guest-a",
        4,
        false,
    )
    .unwrap();
    assert_ne!(fresh.required_owner_approval(), Some(token.clone()));
    assert!(!root.join("runs").exists());
    let run = deploy::register_approved_run(plan, Some(&token)).unwrap();
    let meta = registry::read_meta(&run.path);
    let vm_meta: serde_json::Map<String, Value> = meta
        .iter()
        .filter(|(key, _)| key.starts_with("vm_"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let bytes = String::from_utf8(crate::drift::to_pretty_1space(&Value::Object(vm_meta)).unwrap())
        .unwrap();
    assert_eq!(bytes, include_str!("fixtures/approved-plan.json"));
    assert_eq!(meta["vm_owner_approval"], token);
    assert_eq!(meta["vm_owner_plan"]["requestedGuests"], json!(["guest-a"]));
    assert!(run.path.join("host-a.jsonl").exists());
    assert!(!run.path.join("guest-a.jsonl").exists());
    assert!(!run.path.join("vm").exists());
    std::fs::remove_dir_all(root).unwrap();
}
