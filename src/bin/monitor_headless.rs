use std::collections::{HashSet, HashMap};
use std::fs::File;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ethers::types::H160;
use hyperliquid_rust_sdk::{BaseUrl, InfoClient};
use tokio::runtime::Runtime;
use tokio::time;
use serde::{Deserialize, Serialize};
use regex::Regex;
use reqwest::header::{CONTENT_TYPE, CONTENT_LENGTH};
use chrono;
use serde_urlencoded;

// 必要的结构体定义
#[derive(Debug, Clone, PartialEq)]
enum MonitorType {
    Transactions,
    Perpetuals
}

#[derive(Debug, Serialize, Deserialize)]
struct Monitor {
    address: String,
    monitor_type: String, // "transactions" or "perpetuals"
    active: bool,
}

struct Transaction {
    timestamp: i64,
    token: String,
    side: String,
    size: f64,
    leverage: f64,
    entry_price: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    monitors: Vec<Monitor>,
    sendkeys: Vec<String>,
}

// 主函数
#[tokio::main]
async fn main() {
    println!("Starting Hyperliquid Headless Monitor...");
    
    // 从配置文件加载设置
    let config = load_config().unwrap_or_else(|_| {
        let default_config = Config {
            monitors: Vec::new(),
            sendkeys: Vec::new(),
        };
        save_config(&default_config).expect("Failed to create default config");
        default_config
    });
    
    // 创建共享的事务列表
    let transactions = Arc::new(Mutex::new(Vec::<Transaction>::new()));
    
    // 启动监控任务
    let rt = Runtime::new().unwrap();
    
    for monitor in config.monitors {
        if monitor.active {
            let addr = monitor.address.clone();
            let monitor_type = match monitor.monitor_type.as_str() {
                "transactions" => MonitorType::Transactions,
                "perpetuals" => MonitorType::Perpetuals,
                _ => MonitorType::Transactions,
            };
            let txs = transactions.clone();
            let keys = config.sendkeys.clone();
            
            rt.spawn(async move {
                monitor_address(addr, monitor_type, txs, keys).await;
            });
            
            println!("Started monitoring {} for {}", 
                     monitor.monitor_type, monitor.address);
        }
    }
    
    // 保持程序运行
    loop {
        time::sleep(Duration::from_secs(60)).await;
        println!("Monitor running... Press Ctrl+C to exit");
    }
}

// 监控功能的实现
async fn monitor_address(
    address: String, 
    monitor_type: MonitorType,
    transactions: Arc<Mutex<Vec<Transaction>>>,
    sendkeys: Vec<String>,
) {
    let info_client = InfoClient::new(None, Some(BaseUrl::Mainnet)).await.unwrap();
    let addr: H160 = address.parse().unwrap();
    let mut last_transaction_ids = HashSet::new();
    let start_time = chrono::Utc::now().timestamp_millis();
    
    println!("Starting monitoring for address: {}", address);
    println!("Monitor type: {:?}", monitor_type);
    
    // 根据监控类型初始化
    match monitor_type {
        MonitorType::Transactions => {
            // 初始化交易监控
            if let Ok(fills) = info_client.user_fills(addr).await {
                for fill in fills {
                    let transaction_id = format!("{}-{}", fill.time, fill.oid);
                    last_transaction_ids.insert(transaction_id);
                    
                    // 记录最近1小时的历史交易到内存，但不发送通知
                    if fill.time as i64 > start_time - 3600000 {
                        let tx = Transaction {
                            timestamp: fill.time as i64,
                            token: fill.coin,
                            side: fill.side,
                            size: fill.sz.parse::<f64>().unwrap_or(0.0),
                            leverage: 1.0,
                            entry_price: fill.px.parse::<f64>().unwrap_or(0.0),
                        };
                        
                        let mut txs = transactions.lock().unwrap();
                        txs.push(tx);
                    }
                }
                println!("Initialized with {} existing transactions", last_transaction_ids.len());
            } else {
                println!("Failed to fetch initial transaction data");
            }
        },
        MonitorType::Perpetuals => {
            // 初始化永续合约监控
            let mut last_positions: HashMap<String, f64> = HashMap::new();
            if let Ok(perp_positions) = info_client.user_state(addr).await {
                for position in &perp_positions.asset_positions {
                    let position_data = &position.position;
                    println!("Initial position: {:?}", position_data);
                    
                    last_positions.insert(
                        position.position.coin.clone(),
                        1.0 // 替换为实际字段
                    );
                }
                println!("Initialized with {} existing positions", last_positions.len());
            } else {
                println!("Failed to fetch initial position data");
            }
        }
    }
    
    println!("Monitoring started at {}", to_beijing_time(start_time));
    
    // 主监控循环
    loop {
        time::sleep(Duration::from_secs(10)).await;
        
        match monitor_type {
            MonitorType::Transactions => {
                // 监控交易
                if let Ok(fills) = info_client.user_fills(addr).await {
                    let mut new_transactions = Vec::new();
                    
                    for fill in fills {
                        let transaction_id = format!("{}-{}", fill.time, fill.oid);
                        
                        if !last_transaction_ids.contains(&transaction_id) && (fill.time as i64) > start_time {
                            last_transaction_ids.insert(transaction_id);
                            
                            let tx = Transaction {
                                timestamp: fill.time as i64,
                                token: fill.coin,
                                side: fill.side,
                                size: fill.sz.parse::<f64>().unwrap_or(0.0),
                                leverage: 1.0,
                                entry_price: fill.px.parse::<f64>().unwrap_or(0.0),
                            };
                            
                            new_transactions.push(tx);
                        }
                    }
                    
                    // 处理新交易
                    for tx in &new_transactions {
                        if !sendkeys.is_empty() {
                            let beijing_time = to_beijing_time(tx.timestamp);
                            let time_str = beijing_time.format("%Y-%m-%d %H:%M:%S").to_string();
                            
                            let text = format!("T:{} {} {}", time_str, tx.token, tx.side);
                            let desp = format!(
                                "Address: {}\nToken: {}\nSide: {}\nSize: {}\nPrice: {}\nTime: {}",
                                address, tx.token, tx.side, tx.size, tx.entry_price, time_str
                            );
                            
                            send_to_all_keys(text, desp, sendkeys.clone()).await;
                            println!("New transaction detected: {} {} {}", time_str, tx.token, tx.side);
                        }
                    }
                    
                    // 更新交易列表
                    if !new_transactions.is_empty() {
                        let mut txs = transactions.lock().unwrap();
                        txs.extend(new_transactions);
                    }
                } else {
                    println!("Failed to fetch transaction data");
                }
            },
            MonitorType::Perpetuals => {
                // 永续合约监控逻辑
                if let Ok(_perp_positions) = info_client.user_state(addr).await {
                    // 永续合约监控实现
                    println!("Checking perpetual positions...");
                    // 实现永续合约变化检测和通知
                }
            }
        }
    }
}

// 北京时间转换函数
fn to_beijing_time(timestamp_millis: i64) -> chrono::NaiveDateTime {
    let utc_time = chrono::DateTime::from_timestamp_millis(timestamp_millis)
        .unwrap_or_default()
        .naive_utc();
    
    // 添加8小时获取北京时间
    utc_time + chrono::Duration::hours(8)
}

// Server Chan 发送函数
async fn sc_send(text: String, desp: String, key: String) -> Result<String, Box<dyn std::error::Error>> {
    let params = [("text", text), ("desp", desp)];
    let post_data = serde_urlencoded::to_string(params)?;
    
    // 使用正则提取key中的数字部分
    let url = if key.starts_with("sctp") {
        let re = regex::Regex::new(r"sctp(\d+)t")?;
        if let Some(captures) = re.captures(&key) {
            let num = &captures[1]; // 提取捕获的数字部分
            format!("https://{}.push.ft07.com/send/{}.send", num, key)
        } else {
            return Err("Invalid sendkey format for sctp".into());
        }
    } else {
        format!("https://sctapi.ftqq.com/{}.send", key)
    };
    
    let client = reqwest::Client::new();
    let res = client.post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(reqwest::header::CONTENT_LENGTH, post_data.len() as u64)
        .body(post_data)
        .send()
        .await?;
    
    let data = res.text().await?;
    Ok(data)
}

// 向所有key发送通知
async fn send_to_all_keys(text: String, desp: String, keys: Vec<String>) {
    for key in keys {
        if !key.is_empty() {
            match sc_send(text.clone(), desp.clone(), key.clone()).await {
                Ok(_) => println!("Notification sent to key: {}...", &key[0..min(8, key.len())]),
                Err(e) => println!("Failed to send notification: {}", e),
            }
        }
    }
}

// 辅助函数用于字符串截断
fn min(a: usize, b: usize) -> usize {
    if a < b { a } else { b }
}

// 配置文件处理
fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    let mut file = File::open("monitor_config.json")?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    let config: Config = serde_json::from_str(&contents)?;
    Ok(config)
}

fn save_config(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(config)?;
    let mut file = File::create("monitor_config.json")?;
    file.write_all(json.as_bytes())?;
    Ok(())
}
