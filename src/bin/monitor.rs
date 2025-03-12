use eframe::egui;
use egui::{Color32, Vec2};
use ethers::types::H160;
use hyperliquid_rust_sdk::{BaseUrl, InfoClient};
use regex::Regex;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use std::collections::{HashSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::time;

// Monitor structure
#[derive(Debug, Clone, PartialEq)]
enum MonitorType {
    Transactions,
    Perpetuals
}

struct Monitor {
    address: String,
    monitor_type: MonitorType,  // 改为单选类型
    active: bool,
}

// Transaction information
struct Transaction {
    timestamp: i64,
    token: String,
    side: String,
    size: f64,
    leverage: f64,
    entry_price: f64,
}

// Application state
struct MonitorApp {
    addresses: Vec<Monitor>,
    new_address: String,
    search_query: String,        // 新增：搜索查询
    transactions: Arc<Mutex<Vec<Transaction>>>,
    runtime: Runtime,
    sendkeys: Vec<String>,
    new_sendkey: String,
    sender: Option<mpsc::Sender<String>>,
    selected_monitor_type: MonitorType, // 新增：当前选择的监控类型
}

impl Default for MonitorApp {
    fn default() -> Self {
        let rt = Runtime::new().unwrap();
        Self {
            addresses: Vec::new(),
            new_address: String::new(),
            search_query: String::new(),  // 初始化搜索字段
            transactions: Arc::new(Mutex::new(Vec::new())),
            runtime: rt,
            sendkeys: Vec::new(),
            new_sendkey: String::new(),
            sender: None,
            selected_monitor_type: MonitorType::Transactions, // 默认选择
        }
    }
}

// Send notification
async fn sc_send(text: String, desp: String, key: String) -> Result<String, Box<dyn std::error::Error>> {
    let params = [("text", text), ("desp", desp)];
    let post_data = serde_urlencoded::to_string(params)?;
    // Use regex to extract the numeric part of the key
    let url = if key.starts_with("sctp") {
        let re = Regex::new(r"sctp(\d+)t")?;
        if let Some(captures) = re.captures(&key) {
            let num = &captures[1]; // Extract the captured numeric part
            format!("https://{}.push.ft07.com/send/{}.send", num, key)
        } else {
            return Err("Invalid sendkey format for sctp".into());
        }
    } else {
        format!("https://sctapi.ftqq.com/{}.send", key)
    };
    let client = reqwest::Client::new();
    let res = client.post(&url)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(CONTENT_LENGTH, post_data.len() as u64)
        .body(post_data)
        .send()
        .await?;
    let data = res.text().await?;
    Ok(data)
}

// Send notification to all keys
async fn send_to_all_keys(text: String, desp: String, keys: Vec<String>) {
    for key in keys {
        if !key.is_empty() {
            let _ = sc_send(text.clone(), desp.clone(), key).await;
        }
    }
}

// Helper function to convert a timestamp to Beijing time (UTC+8)
fn to_beijing_time(timestamp_millis: i64) -> chrono::NaiveDateTime {
    let utc_time = chrono::DateTime::from_timestamp_millis(timestamp_millis)
        .unwrap_or_default()
        .naive_utc();
    
    // Add 8 hours for Beijing time (UTC+8)
    utc_time + chrono::Duration::hours(8)
}

// Monitoring logic
async fn monitor_address(
    address: String, 
    monitor_type: MonitorType,
    transactions: Arc<Mutex<Vec<Transaction>>>,
    sendkeys: Vec<String>,
) {
    let info_client = InfoClient::new(None, Some(BaseUrl::Mainnet)).await.unwrap();
    let addr: H160 = address.parse().unwrap();
    let mut _last_check = Instant::now();
    let mut last_transaction_ids = HashSet::new();
    let start_time = chrono::Utc::now().timestamp_millis();
    
    // 根据监控类型初始化
    match monitor_type {
        MonitorType::Transactions => {
            // 初始化交易监控
            if let Ok(fills) = info_client.user_fills(addr).await {
                for fill in fills {
                    let transaction_id = format!("{}-{}", fill.time, fill.oid);
                    last_transaction_ids.insert(transaction_id);
                    
                    // 添加最近1小时的历史记录到UI
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
                        1.0 // 需替换为实际字段
                    );
                }
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
                        }
                    }
                    
                    // 更新交易列表
                    if !new_transactions.is_empty() {
                        let mut txs = transactions.lock().unwrap();
                        txs.extend(new_transactions);
                    }
                }
            },
            MonitorType::Perpetuals => {
                // 永续合约监控逻辑
                // ...
            }
        }
    }
}

impl eframe::App for MonitorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 设置全局样式
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(245, 245, 250);
        style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(220, 220, 235);
        ctx.set_style(style);

        // 使用左右分栏布局
        egui::SidePanel::left("control_panel")
            .resizable(true)
            .default_width(300.0)
            .min_width(250.0)
            .show(ctx, |ui| {
                ui.add_space(5.0);
                ui.heading("Monitoring Controls");
                ui.add_space(10.0);
                
                // 地址添加区域
                egui::Frame::none()
                    .fill(Color32::from_rgb(230, 230, 240))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::style::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.heading("Add Address to Monitor");
                        ui.add_space(5.0);
                        
                        ui.label("Address:");
                        ui.text_edit_singleline(&mut self.new_address)
                            .on_hover_text("Enter Hyperliquid wallet address");
                        
                        // 添加监控类型单选按钮
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut self.selected_monitor_type, MonitorType::Transactions, "Monitor Transactions");
                            ui.radio_value(&mut self.selected_monitor_type, MonitorType::Perpetuals, "Monitor Perpetuals");
                        });
                        
                        // 添加地址时检查重复
                        let mut error_msg = None;
                        if ui.add(egui::Button::new("Add Monitor")
                            .fill(Color32::from_rgb(100, 150, 220)))
                            .clicked() && !self.new_address.is_empty() {
                            
                            // 检查地址是否已存在
                            let address_exists = self.addresses.iter()
                                .any(|m| m.address.to_lowercase() == self.new_address.to_lowercase());
                            
                            if address_exists {
                                error_msg = Some("This address is already being monitored");
                            } else {
                                self.addresses.push(Monitor {
                                    address: self.new_address.clone(),
                                    monitor_type: self.selected_monitor_type.clone(),
                                    active: false,
                                });
                                self.new_address.clear();
                            }
                        }
                        
                        // 显示错误消息
                        if let Some(msg) = error_msg {
                            ui.add_space(5.0);
                            ui.colored_label(Color32::from_rgb(220, 60, 60), msg);
                        }
                    });
                
                ui.add_space(15.0);
                
                // Server Chan 设置区域
                egui::Frame::none()
                    .fill(Color32::from_rgb(230, 230, 240))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::style::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.heading("Notification Settings");
                        ui.add_space(5.0);
                        
                        ui.label("Server Chan SendKey:");
                        ui.text_edit_singleline(&mut self.new_sendkey)
                            .on_hover_text("Enter Server Chan API key");
                        
                        if ui.add(egui::Button::new("Add Key")
                            .fill(Color32::from_rgb(100, 150, 220)))
                            .clicked() && !self.new_sendkey.is_empty() {
                            self.sendkeys.push(self.new_sendkey.clone());
                            self.new_sendkey.clear();
                        }
                        
                        // 已注册的keys列表
                        if !self.sendkeys.is_empty() {
                            ui.add_space(10.0);
                            ui.label("Registered Keys:");
                            
                            let mut to_remove_key = None;
                            for (i, key) in self.sendkeys.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    // 显示部分隐藏的key，保护隐私
                                    let display_key = if key.len() > 8 {
                                        format!("{}...{}", &key[0..4], &key[key.len()-4..])
                                    } else {
                                        key.clone()
                                    };
                                    
                                    ui.label(format!("{}.", i+1));
                                    ui.label(display_key);
                                    
                                    if ui.add(egui::Button::new("✕")
                                        .fill(Color32::from_rgb(220, 100, 100))
                                        .small())
                                        .clicked() {
                                        to_remove_key = Some(i);
                                    }
                                });
                            }
                            
                            if let Some(idx) = to_remove_key {
                                self.sendkeys.remove(idx);
                            }
                        }
                    });
                
                ui.add_space(15.0);
                
                // 监控地址列表 - 添加搜索功能
                egui::Frame::none()
                    .fill(Color32::from_rgb(230, 230, 240))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::style::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.heading("Active Monitors");
                        ui.add_space(5.0);
                        
                        // 添加搜索栏
                        ui.horizontal(|ui| {
                            ui.label("Search:");
                            ui.text_edit_singleline(&mut self.search_query);
                        });
                        
                        ui.add_space(10.0);
                        
                        let mut to_remove = None;
                        
                        // 用滚动区域包裹监控列表
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .auto_shrink([false; 2])
                            .show(ui, |ui| {
                                let search_query = self.search_query.to_lowercase();
                                
                                // 筛选符合搜索条件的地址
                                let filtered_addresses: Vec<(usize, &mut Monitor)> = self.addresses.iter_mut()
                                    .enumerate()
                                    .filter(|(_, monitor)| {
                                        search_query.is_empty() || 
                                        monitor.address.to_lowercase().contains(&search_query)
                                    })
                                    .collect();
                                
                                if filtered_addresses.is_empty() {
                                    ui.label("No matching addresses found");
                                    return;
                                }
                                
                                for (i, monitor) in filtered_addresses {
                                    egui::Frame::none()
                                        .fill(if monitor.active {
                                            Color32::from_rgb(220, 250, 220)
                                        } else {
                                            Color32::from_rgb(240, 240, 240)
                                        })
                                        .rounding(egui::Rounding::same(4.0))
                                        .inner_margin(egui::style::Margin::same(8.0))
                                        .show(ui, |ui| {
                                            // 简洁显示地址
                                            let display_addr = if monitor.address.len() > 10 {
                                                format!("{}...{}", 
                                                    &monitor.address[0..6], 
                                                    &monitor.address[monitor.address.len()-4..])
                                            } else {
                                                monitor.address.clone()
                                            };
                                            
                                            ui.label(format!("{}. {}", i+1, display_addr));
                                            
                                            // 显示监控类型
                                            ui.label(match monitor.monitor_type {
                                                MonitorType::Transactions => "Type: Transactions",
                                                MonitorType::Perpetuals => "Type: Perpetuals",
                                            });
                                            
                                            ui.horizontal(|ui| {
                                                if monitor.active {
                                                    if ui.add(egui::Button::new("Stop")
                                                        .fill(Color32::from_rgb(220, 100, 100)))
                                                        .clicked() {
                                                        monitor.active = false;
                                                    }
                                                } else {
                                                    if ui.add(egui::Button::new("Start")
                                                        .fill(Color32::from_rgb(100, 200, 100)))
                                                        .clicked() {
                                                        let addr = monitor.address.clone();
                                                        let monitor_type = monitor.monitor_type.clone();
                                                        let txs = self.transactions.clone();
                                                        let keys = self.sendkeys.clone();
                                                        
                                                        self.runtime.spawn(async move {
                                                            monitor_address(addr, monitor_type, txs, keys).await;
                                                        });
                                                        
                                                        monitor.active = true;
                                                    }
                                                }
                                                
                                                if ui.add(egui::Button::new("Delete")
                                                    .fill(Color32::from_rgb(200, 120, 120))
                                                    .small())
                                                    .clicked() {
                                                    to_remove = Some(i);
                                                }
                                            });
                                        });
                                    ui.add_space(5.0);
                                }
                            });
                        
                        if let Some(index) = to_remove {
                            self.addresses.remove(index);
                        }
                    });
            });
        
        // 右侧交易记录面板
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(5.0);
            ui.heading("Hyperliquid Transaction Monitor");
            ui.add_space(15.0);
            
            // 交易记录卡片
            egui::Frame::none()
                .fill(Color32::from_rgb(235, 235, 240))
                .rounding(egui::Rounding::same(8.0))
                .inner_margin(egui::style::Margin::same(10.0))
                .show(ui, |ui| {
                    // 表头
                    ui.horizontal(|ui| {
                        ui.heading("Recent Transactions");
                        ui.add_space(10.0);
                        ui.label("(All times in UTC+8)");
                    });
                    
                    ui.add_space(5.0);
                    
                    // 表格式表头
                    ui.horizontal(|ui| {
                        let col_width1 = 180.0;
                        let col_width2 = 80.0;
                        
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                            ui.set_width(col_width1);
                            ui.label("Time");
                        });
                        
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                            ui.set_width(col_width2);
                            ui.label("Token");
                        });
                        
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                            ui.set_width(col_width2);
                            ui.label("Side");
                        });
                        
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                            ui.set_width(col_width2);
                            ui.label("Size");
                        });
                        
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                            ui.set_width(col_width2);
                            ui.label("Leverage");
                        });
                        
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                            ui.set_width(col_width2);
                            ui.label("Price");
                        });
                    });
                    
                    ui.separator();
                    
                    // 交易列表
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            let txs = self.transactions.lock().unwrap();
                            
                            if txs.is_empty() {
                                ui.add_space(20.0);
                                ui.centered_and_justified(|ui| {
                                    ui.label("No transactions recorded yet");
                                });
                                return;
                            }
                            
                            // 按时间倒序显示
                            for tx in txs.iter().rev() {
                                let time = to_beijing_time(tx.timestamp);
                                let time_str = time.format("%Y-%m-%d %H:%M:%S").to_string();
                                
                                egui::Frame::none()
                                    .fill(Color32::from_rgb(245, 245, 250))
                                    .inner_margin(egui::style::Margin::same(8.0))
                                    .rounding(egui::Rounding::same(4.0))
                                    .show(ui, |ui| {
                                        let col_width1 = 180.0;
                                        let col_width2 = 80.0;
                                        
                                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                                            ui.set_width(col_width1);
                                            ui.label(time_str);
                                        });
                                        
                                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                                            ui.set_width(col_width2);
                                            ui.label(&tx.token);
                                        });
                                        
                                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                                            ui.set_width(col_width2);
                                            let side_color = if tx.side == "long" || tx.side == "buy" {
                                                Color32::from_rgb(50, 150, 50)
                                            } else {
                                                Color32::from_rgb(180, 50, 50)
                                            };
                                            ui.colored_label(side_color, &tx.side);
                                        });
                                        
                                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                                            ui.set_width(col_width2);
                                            ui.label(format!("{:.4}", tx.size));
                                        });
                                        
                                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                                            ui.set_width(col_width2);
                                            ui.label(format!("{:.2}x", tx.leverage));
                                        });
                                        
                                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(false), |ui| {
                                            ui.set_width(col_width2);
                                            ui.label(format!("{:.4}", tx.entry_price));
                                        });
                                    });
                                ui.add_space(4.0);
                            }
                        });
                });
        });
    }
}

#[tokio::main]
async fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        initial_window_size: Some(Vec2::new(800.0, 600.0)),
        ..Default::default()
    };
    eframe::run_native(
        "Hyperliquid Monitor Tool",
        options,
        Box::new(|_cc| Box::new(MonitorApp::default()))
    )
} 