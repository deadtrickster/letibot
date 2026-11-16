//! The account probe's rule, read off a body and off a loopback door: no plan, no network, no turn.

use super::*;

/// z.ai's own answer to a call on a plan with nothing on it - the case the probe exists for.
const NO_PLAN: &str =
    r#"{"error":{"code":1113,"message":"您的账户余额不足或无可用资源包，请充值"}}"#;

/// A plan that can serve: its windows, as the quota door reports them.
const A_PLAN: &str = r#"{"code":200,"success":true,"data":{"limits":[
    {"type":"TOKENS_LIMIT","unit":5,"number":1,"percentage":41}]}}"#;

#[test]
fn a_plan_with_no_balance_is_refused_by_name() {
    let body: serde_json::Value = serde_json::from_str(NO_PLAN).unwrap();
    let said = match judge("glm", Some("glm-5.3"), &body) {
        Account::CannotServe(said) => said,
        other => panic!("a plan with no balance cannot serve: {other:?}"),
    };
    assert!(said.contains("`glm/glm-5.3`"), "the model is named: {said}");
    assert!(
        said.contains("余额") || said.contains("1113") || said.contains("cannot serve"),
        "{said}"
    );
}

#[test]
fn a_plan_with_money_on_it_is_ready_and_a_body_nothing_reads_judges_nothing() {
    let body: serde_json::Value = serde_json::from_str(A_PLAN).unwrap();
    assert_eq!(judge("glm", Some("glm-5.3"), &body), Account::Ready);
    // A body with none of the words says nothing about the account, so it refuses nothing.
    let odd = serde_json::json!({ "hello": "world" });
    assert_eq!(judge("glm", None, &odd), Account::Ready);
    // And the prose form: a provider that says it in words and no code at all.
    let prose = serde_json::json!({"error": "insufficient balance on this account"});
    assert!(matches!(
        judge("deepseek", None, &prose),
        Account::CannotServe(_)
    ));
}

/// The probe is one request, and a door that is not there is no fact about the account: a probe
/// that fails must not refuse a start.
#[test]
fn a_dead_door_is_unknown_and_refuses_nothing() {
    let creds = letibot_provider::keys::Credentials {
        key: "sk-test".into(),
        from: "a test".into(),
        prices: Default::default(),
        url: None,
    };
    let dead = letibot_provider::usage_poll::body("http://127.0.0.1:1/usage", &creds);
    assert!(dead.is_none(), "nothing answered on that port");
    let said = ask(
        "http://127.0.0.1:1/usage",
        letibot_provider::registry::lookup("glm-coding", None).expect("a preset this build owns"),
        &creds,
        None,
    );
    assert!(matches!(said, Account::Unknown(_)), "{said:?}");
    assert_eq!(
        refusal("this-provider-does-not-exist", None, None),
        None,
        "an unknown provider is not a refusal either"
    );
}

#[test]
fn a_glm_account_is_asked_on_zaiis_quota_door_and_a_stranger_is_not() {
    assert_eq!(
        plan_door("https://open.bigmodel.cn/api/paas/v4/chat/completions").as_deref(),
        Some("https://open.bigmodel.cn/api/monitor/usage/quota/limit"),
        "the plain glm door's plan is reported on the same host and key"
    );
    assert_eq!(
        plan_door("https://api.z.ai/api/paas/v4/chat/completions").as_deref(),
        Some("https://api.z.ai/api/monitor/usage/quota/limit")
    );
    assert_eq!(
        plan_door("https://api.deepseek.com/chat/completions"),
        None,
        "a door whose plan publishes nothing is not asked"
    );
}

/// The answer is kept per account: a fleet of starts in one process asks the plan once.
#[test]
fn the_verdict_is_kept_for_its_freshness() {
    let at = format!("test-{}/abcdefgh", std::process::id());
    assert_eq!(kept(&at), None);
    keep(&at, Account::CannotServe("no balance".into()));
    assert_eq!(
        kept(&at),
        Some(Account::CannotServe("no balance".into())),
        "a second start reads the kept answer"
    );
    assert_eq!(kept("another-account/abcdefgh"), None, "keyed by account");
}
