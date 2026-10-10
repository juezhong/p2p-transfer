//! Transfer only presents pairing UI; SDK owns NIC/STUN/ICE/NAT/mTLS/QUIC.
use std::{net::SocketAddr, sync::Arc};

use p2p_sdk::{
    begin_creator_auto, begin_joiner_auto,
    peer_pin::ManualConfirmation,
};
use transfer_core::{
    secure_io::SharedRoot,
    transport_lanes::{TransferDataLanes, TransferSession},
};

use crate::{debug_error, prompt, CliResult};

const TRANSFER_DATA_CONNECTIONS: usize = 4;

pub struct ActivePeer {
    pub session: Arc<TransferSession>,
    pub data_lanes: Arc<TransferDataLanes>,
    pub peer_address: SocketAddr,
    pub local_address: SocketAddr,
    pub role: &'static str,
}

fn verify_user(mut confirmation: ManualConfirmation) -> CliResult<ManualConfirmation> {
    println!("双方设备应显示相同的 6 位配对核对码：{}",
        confirmation.comparison_code_text());
    println!("请通过另一可信渠道（例如语音）核对两台设备的数字是否一致。");
    println!("这不是邀请码的一部分；它用于阻止首次配对被第三方替换。");
    if prompt("已经与另一台设备核对且完全一致？输入 yes 继续，其余输入取消：")?
        != "yes"
    {
        return Err("用户未核对双方配对码，拒绝建立连接".to_owned());
    }
    confirmation.confirm(&confirmation.comparison_code_text())
        .map_err(|_| "配对核对未通过".to_owned())?;
    Ok(confirmation)
}

fn connected(
    transport: p2p_sdk::ConnectedTransportPeer, role: &'static str,
) -> ActivePeer {
    let info = transport.diagnostic();
    let transport = Arc::new(transport);
    // Transfer 的四路是应用策略，SDK 只为每条连接负责认证与恢复。
    let data_lanes = Arc::new(TransferDataLanes::new(
        &transport, TRANSFER_DATA_CONNECTIONS,
    ));
    let session = Arc::new(TransferSession::new(transport, Arc::clone(&data_lanes)));
    ActivePeer {
        session, data_lanes,
        peer_address: info.actual_remote_udp,
        local_address: info.actual_local_udp, role,
    }
}

pub async fn run() -> CliResult<()> {
    println!("\nP2P Transfer（Rust）— 交互模式");
    println!("1) 创建连接（生成 INVITE 邀请码）");
    println!("2) 加入连接（输入 INVITE 邀请码）");
    println!("3) 退出");
    let mode = loop {
        match prompt("请选择 [1/2/3]：")?.as_str() {
            "1" | "create" => break 1,
            "2" | "join" => break 2,
            "3" | "quit" | "exit" => return Ok(()),
            _ => println!("请输入 1、2 或 3"),
        }
    };
    let dir = std::env::current_dir().map_err(debug_error)?;
    println!("\n本次连接将共享的本机目录：{}", dir.display());
    println!("对端只能访问明确授权的共享目录及其子目录。");
    if prompt(&format!("确认允许对端访问【{}】及其子目录？输入 yes 授权：",
        dir.display()))? != "yes"
    {
        return Err("未授权目录；会话未建立".into());
    }
    let root = Arc::new(SharedRoot::authorize(&dir).map_err(debug_error)?);
    let peer = if mode == 1 {
        create_connection().await?
    } else {
        join_connection().await?
    };
    println!("已建立安全 P2P 连接。输入 help 查看命令。");
    crate::shell::run(peer, root).await
}

async fn create_connection() -> CliResult<ActivePeer> {
    let (pending, invite) = begin_creator_auto().await.map_err(debug_error)?;
    println!("把下面的 INVITE 邀请码私下发给对方：\n{invite}");
    let reply = prompt("请粘贴对方返回的 REPLY 回传码：")?;
    let ready = pending.receive_reply_now(&reply).map_err(debug_error)?;
    let confirmation = verify_user(ready.confirmation().map_err(debug_error)?)?;
    let transport = ready.connect_transport_now(&confirmation).await.map_err(debug_error)?;
    let info = transport.diagnostic();
    println!("直连 ICE 提名：{} → {}，无服务器中继",
        info.actual_local_udp, info.actual_remote_udp);
    Ok(connected(transport, "创建方"))
}

async fn join_connection() -> CliResult<ActivePeer> {
    let invite = prompt("请粘贴创建方发送的 INVITE 邀请码：")?;
    let (ready, reply) = begin_joiner_auto(&invite).await.map_err(debug_error)?;
    println!("把下面的 REPLY 回传码私下发给创建方：\n{reply}");
    let confirmation = verify_user(ready.confirmation().map_err(debug_error)?)?;
    let transport = ready.connect_transport_now(&confirmation).await.map_err(debug_error)?;
    let info = transport.diagnostic();
    println!("直连 ICE 提名：{} → {}，无服务器中继",
        info.actual_local_udp, info.actual_remote_udp);
    Ok(connected(transport, "加入方"))
}
