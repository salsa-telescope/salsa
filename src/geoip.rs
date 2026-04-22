use db_ip::{CountryCode, DbIpDatabase, include_country_code_database};
use std::net::IpAddr;
use std::sync::OnceLock;

static DB: OnceLock<DbIpDatabase<CountryCode>> = OnceLock::new();

pub fn lookup_country(ip: IpAddr) -> Option<String> {
    let db = DB.get_or_init(|| include_country_code_database!());
    db.get(&ip).map(|c| c.to_string())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn google_dns_is_us() {
        let ip: IpAddr = "8.8.8.8".parse().unwrap();
        assert_eq!(lookup_country(ip).as_deref(), Some("US"));
    }

    #[test]
    fn lund_university_is_se() {
        let ip: IpAddr = "130.235.0.1".parse().unwrap();
        assert_eq!(lookup_country(ip).as_deref(), Some("SE"));
    }
}
