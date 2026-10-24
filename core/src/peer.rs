//! 本機 TCP 連線的另一頭是誰：錄音管線、單一執行個體的控制端點、Jellyfin 轉送用來確認
//! 連過來的（或在聽某個連接埠的）是自己這個登入工作階段的程式，不是同一台電腦上其他使用者的。

/// 本機 TCP 連線是哪個程序開的：peer 是對方的位址，port 是我們這邊的埠。查不到時 None
#[cfg(windows)]
pub fn tcp_owner(peer: std::net::SocketAddr, port: u16) -> Option<u32> {
    use windows::Win32::NetworkManagement::IpHelper::TCP_TABLE_OWNER_PID_CONNECTIONS;
    let be = |p: u32| u16::from_be(p as u16);
    tcp_rows(TCP_TABLE_OWNER_PID_CONNECTIONS)?.into_iter().find(|r| be(r.0) == peer.port() && be(r.1) == port).map(|r| r.2)
}

#[cfg(not(windows))]
pub fn tcp_owner(_: std::net::SocketAddr, _: u16) -> Option<u32> {
    None
}

/// 在本機 127.0.0.1 這個連接埠上聽的是哪個程序。查不到時 None
#[cfg(windows)]
pub fn listener_owner(port: u16) -> Option<u32> {
    use windows::Win32::NetworkManagement::IpHelper::TCP_TABLE_OWNER_PID_LISTENER;
    tcp_rows(TCP_TABLE_OWNER_PID_LISTENER)?.into_iter().find(|r| u16::from_be(r.0 as u16) == port).map(|r| r.2)
}

#[cfg(not(windows))]
pub fn listener_owner(_: u16) -> Option<u32> {
    None
}

/// IPv4 TCP 表：（本機埠、對方埠、程序編號），都是表裡的原始值（埠是網路位元組順序）
#[cfg(windows)]
fn tcp_rows(class: windows::Win32::NetworkManagement::IpHelper::TCP_TABLE_CLASS) -> Option<Vec<(u32, u32, u32)>> {
    use windows::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, MIB_TCPROW_OWNER_PID};
    const AF_INET: u32 = 2;
    let mut size = 0u32;
    // SAFETY: 先問要多大，再用夠大的緩衝（u32 對齊）讀整張表；列數不超過緩衝
    unsafe {
        GetExtendedTcpTable(None, &mut size, false, AF_INET, class, 0);
        for _ in 0..3 {
            let mut buf = vec![0u32; size as usize / 4 + 64];
            let mut len = (buf.len() * 4) as u32;
            match GetExtendedTcpTable(Some(buf.as_mut_ptr().cast()), &mut len, false, AF_INET, class, 0) {
                0 => {
                    let n = (buf[0] as usize).min((buf.len() - 1) / 6);
                    let rows = std::slice::from_raw_parts(buf.as_ptr().add(1) as *const MIB_TCPROW_OWNER_PID, n);
                    // 編號 0：系統沒給（Wine 等），當作查不到
                    return Some(rows.iter().filter(|r| r.dwOwningPid != 0).map(|r| (r.dwLocalPort, r.dwRemotePort, r.dwOwningPid)).collect());
                }
                // 表變大了：重來
                122 => size = len,
                _ => return None,
            }
        }
        None
    }
}

/// 這個程序和本程式在同一個登入工作階段（同一個使用者的桌面）。查不到時 None
#[cfg(windows)]
pub fn same_session(pid: u32) -> Option<bool> {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let (mut theirs, mut ours) = (0u32, 0u32);
    // SAFETY: 只讀兩個程序的工作階段編號
    unsafe {
        ProcessIdToSessionId(pid, &mut theirs).ok()?;
        ProcessIdToSessionId(std::process::id(), &mut ours).ok()?;
    }
    Some(theirs == ours)
}

#[cfg(not(windows))]
pub fn same_session(_: u32) -> Option<bool> {
    None
}

/// 連過來的是同一個登入工作階段的程式（查不到時放行：Wine、權限不夠讀表時不擋）
pub fn peer_ok(peer: std::net::SocketAddr, port: u16) -> bool {
    tcp_owner(peer, port).and_then(same_session) != Some(false)
}
