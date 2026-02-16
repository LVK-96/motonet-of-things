//! NTP server list parsing for firmware configuration.

use core::net::Ipv4Addr;

pub const MAX_NTP_SERVERS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NtpServerList {
    servers: [Ipv4Addr; MAX_NTP_SERVERS],
    len: usize,
}

impl NtpServerList {
    #[must_use]
    pub fn as_slice(&self) -> &[Ipv4Addr] {
        &self.servers[..self.len]
    }
}

#[must_use]
pub fn parse_ntp_server_list(raw: &str, fallback: &[Ipv4Addr]) -> NtpServerList {
    let mut servers = [Ipv4Addr::UNSPECIFIED; MAX_NTP_SERVERS];
    let mut len = 0;

    for candidate in raw.split(',') {
        if len == MAX_NTP_SERVERS {
            break;
        }

        let trimmed = candidate.trim();
        if let Ok(addr) = trimmed.parse::<Ipv4Addr>()
            && !servers[..len].contains(&addr)
        {
            servers[len] = addr;
            len += 1;
        }
    }

    if len == 0 {
        for addr in fallback {
            if len == MAX_NTP_SERVERS || servers[..len].contains(addr) {
                continue;
            }
            servers[len] = *addr;
            len += 1;
        }
    }

    NtpServerList { servers, len }
}

#[cfg(test)]
mod tests {
    use super::{MAX_NTP_SERVERS, parse_ntp_server_list};
    use core::net::Ipv4Addr;

    #[test]
    fn parses_configured_ntp_servers_in_order() {
        let fallback = [Ipv4Addr::new(129, 6, 15, 28)];
        let parsed = parse_ntp_server_list("216.239.35.0, 216.239.35.4", &fallback);
        assert_eq!(
            parsed.as_slice(),
            &[
                Ipv4Addr::new(216, 239, 35, 0),
                Ipv4Addr::new(216, 239, 35, 4)
            ]
        );
    }

    #[test]
    fn skips_invalid_entries_and_falls_back_when_empty() {
        let fallback = [
            Ipv4Addr::new(129, 6, 15, 28),
            Ipv4Addr::new(132, 163, 97, 1),
        ];
        let parsed = parse_ntp_server_list("invalid, 300.1.1.1, ,", &fallback);
        assert_eq!(parsed.as_slice(), &fallback);
    }

    #[test]
    fn limits_to_max_servers() {
        let parsed = parse_ntp_server_list(
            "1.1.1.1,2.2.2.2,3.3.3.3,4.4.4.4,5.5.5.5",
            &[Ipv4Addr::new(9, 9, 9, 9)],
        );
        assert_eq!(parsed.as_slice().len(), MAX_NTP_SERVERS);
    }

    #[test]
    fn deduplicates_entries() {
        let parsed = parse_ntp_server_list(
            "216.239.35.0,216.239.35.0,216.239.35.4",
            &[Ipv4Addr::new(9, 9, 9, 9)],
        );
        assert_eq!(
            parsed.as_slice(),
            &[
                Ipv4Addr::new(216, 239, 35, 0),
                Ipv4Addr::new(216, 239, 35, 4)
            ]
        );
    }
}
