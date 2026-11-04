use fleet_api::Object;
use fleet_api::plugin::Registration;
use fleet_controllers::plugin::admit;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn registration() -> Registration {
    serde_json::from_value(json!({"manifest":{"protocol":1,"name":"issues","kinds":[{"kind":"Issue","pollMs":1000,"spec":true,"finalizer":true}]},"config":{"program":"fixture","cwd":"fixture"}})).unwrap()
}
fn object(kind: &str, name: &str, uid: &str) -> Object {
    serde_json::from_value(json!({"apiVersion":"fleet/v1alpha1","kind":kind,"metadata":{"name":name,"uid":uid},"spec":{}})).unwrap()
}
fn accepts(plan: Value) -> bool {
    admit(
        plan,
        &registration(),
        &object("Issue", "one", "uid"),
        &BTreeMap::new(),
    )
    .is_ok()
}

#[test]
fn rejects_unknown_verbs_reserved_fields_and_invalid_bounds() {
    for plan in [
        json!({"result":{}}),
        json!({"status":{"effects":{}}}),
        json!({"status":[]}),
        json!({"removeFinalizer":true}),
        json!({"effects":[{"key":"x","request":{}},{"key":"x","request":{}}]}),
        json!({"effects":(0..9).map(|n|json!({"key":n.to_string(),"request":{}})).collect::<Vec<_>>()}),
        json!({"creates":[object("ChangeRequest","forbidden","")]}),
        json!({"creates":[object("Issue","identity","already-assigned")]}),
        json!({"status":{"large":"x".repeat(256*1024)}}),
    ] {
        assert!(!accepts(plan.clone()), "{plan}");
    }
    assert!(accepts(json!({"status":{"phase":"Ready"}})));
}

#[test]
fn task_targets_need_a_known_uid_and_rights_and_only_mutable_spec_fields() {
    let mut task = object("Task", "child", "uid");
    let mut targets = BTreeMap::from([("Task/child".into(), task.clone())]);
    let patch =
        json!({"patches":[{"kind":"Task","name":"child","uid":"uid","spec":{"dispatch":true}}]});
    assert!(
        admit(
            patch.clone(),
            &registration(),
            &object("Issue", "one", "uid"),
            &targets
        )
        .is_err()
    );
    task.metadata
        .labels
        .insert("fleet.dev/plugin".into(), "issues".into());
    targets.insert("Task/child".into(), task);
    assert!(
        admit(
            patch.clone(),
            &registration(),
            &object("Issue", "one", "uid"),
            &targets
        )
        .is_ok()
    );
    let mut stale = patch.clone();
    stale["patches"][0]["uid"] = json!("old");
    assert!(
        admit(
            stale,
            &registration(),
            &object("Issue", "one", "uid"),
            &targets
        )
        .is_err()
    );
    let mut immutable = patch;
    immutable["patches"][0]["spec"] = json!({"brief":"overwrite"});
    assert!(
        admit(
            immutable,
            &registration(),
            &object("Issue", "one", "uid"),
            &targets
        )
        .is_err()
    );
}

#[test]
fn amends_need_the_task_in_scope_a_known_uid_text_and_a_small_count() {
    let amend =
        |text: &str, uid: &str| json!({"task":"child","uid":uid,"text":text,"ifGeneration":1});
    let mut task = object("Task", "child", "uid");
    let mut targets = BTreeMap::from([("Task/child".into(), task.clone())]);
    let check = |plan: Value, targets: &BTreeMap<String, Object>| {
        admit(
            plan,
            &registration(),
            &object("Issue", "one", "uid"),
            targets,
        )
    };
    let one = json!({"amends":[amend("Review the new push.","uid")]});
    let foreign = check(one.clone(), &targets).unwrap_err().to_string();
    assert!(foreign.contains("foreign Task"), "{foreign}");
    task.metadata
        .labels
        .insert("fleet.dev/plugin".into(), "issues".into());
    targets.insert("Task/child".into(), task);
    assert!(check(one, &targets).is_ok());
    for plan in [
        json!({"amends":[amend("Review the new push.","old")]}),
        json!({"amends":[amend("  ","uid")]}),
        json!({"amends":[{"task":"child","uid":"uid","text":"no precondition"}]}),
        json!({"amends":(0..=fleet_controllers::plugin::MAX_AMENDS).map(|_|amend("again","uid")).collect::<Vec<_>>()}),
    ] {
        assert!(check(plan.clone(), &targets).is_err(), "{plan}");
    }
    let mut any = registration();
    any.config.tasks = fleet_api::plugin::TaskAccess::Any;
    let unlabelled = BTreeMap::from([("Task/child".into(), object("Task", "child", "uid"))]);
    assert!(
        admit(
            json!({"amends":[amend("Any Task in scope.","uid")]}),
            &any,
            &object("Issue", "one", "uid"),
            &unlabelled
        )
        .is_ok()
    );
}
