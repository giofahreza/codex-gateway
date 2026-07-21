//! One-off diagnostic for Z.AI balance endpoints.
//! Run with: cargo run --example glm_balance_probe --release
//! Set GLM_API_KEY in the environment.
use serde_json::Value;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let api_key = match std::env::var("GLM_API_KEY") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("Set GLM_API_KEY in the environment");
            std::process::exit(2);
        }
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let hosts = ["api.z.ai", "z.ai", "open.bigmodel.cn"];
    let paths = [
        "/api/finance/balance",
        "/api/account/balance",
        "/api/billing/balance",
        "/api/balance",
    ];
    for host in hosts {
        for path in paths {
            let url = format!("https://{}{}", host, path);
            let resp = client
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Accept", "application/json")
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    let parsed: Option<Value> = serde_json::from_str(&body).ok();
                    let hint = match &parsed {
                        Some(v) if v.get("success").and_then(|x| x.as_bool()) == Some(false) => {
                            format!(" [error: {} {}]",
                                v.get("code").map(|x| x.to_string()).unwrap_or_default(),
                                v.get("msg").and_then(|x| x.as_str()).unwrap_or(""))
                        }
                        Some(v) if parsed.as_ref().and_then(|p| p.get("balance_infos")).is_some() => {
                            " [BALANCE FOUND]".to_string()
                        }
                        _ => String::new(),
                    };
                    println!("{} {}{}\n    status={} body={}",
                        if status.is_success() { "OK" } else { "  " },
                        url, hint, status,
                        body.chars().take(200).collect::<String>());
                }
                Err(e) => println!("ERR {}: {}", url, e),
            }
        }
    }
}
