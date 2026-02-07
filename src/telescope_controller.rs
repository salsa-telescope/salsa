use crate::coords::Direction;
use crate::models::telescope_types::TelescopeError;
use hex_literal::hex;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::str::FromStr;
use std::time::Duration;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TelescopeCommand {
    Stop,
    Restart,
    GetDirection,
    SetDirection(Direction),
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TelescopeResponse {
    Ack,
    CurrentDirection(Direction),
}

pub struct TelescopeController {
    // FIXME: Do we need to be able to mock at this level?
    stream: TcpStream,
}

impl TelescopeController {
    pub fn connect(address: &str) -> Result<TelescopeController, TelescopeError> {
        let stream = create_connection(address)?;
        Ok(TelescopeController { stream })
    }

    pub fn execute(
        &mut self,
        command: TelescopeCommand,
    ) -> Result<TelescopeResponse, TelescopeError> {
        self.stream
            .write_all(&command.to_bytes())
            .map_err(|err| TelescopeError::TelescopeIOError(err.to_string()))?;
        let mut result = vec![0; 128];
        let response_length = self
            .stream
            .read(&mut result)
            .map_err(|err| TelescopeError::TelescopeIOError(err.to_string()))?;
        result.truncate(response_length);
        command.parse_response(&result)
    }
}

impl TelescopeCommand {
    fn to_bytes(self) -> Vec<u8> {
        match self {
            TelescopeCommand::Stop => hex!("57000000000000000000000F20").into(),
            TelescopeCommand::Restart => hex!("57EFBEADDE000000000000EE20").into(),
            TelescopeCommand::GetDirection => hex!("57000000000000000000006F20").into(),
            TelescopeCommand::SetDirection(direction) => {
                let mut bytes = Vec::with_capacity(13);
                bytes.extend(hex!("57"));
                bytes.extend(rot2prog_angle_to_bytes(direction.azimuth).as_slice());
                bytes.extend(rot2prog_angle_to_bytes(direction.elevation).as_slice());
                bytes.extend(hex!("5F20"));
                bytes
            }
        }
    }

    fn parse_response(&self, bytes: &[u8]) -> Result<TelescopeResponse, TelescopeError> {
        match self {
            TelescopeCommand::Stop => parse_ack_response(bytes, "stop"),
            TelescopeCommand::Restart => parse_ack_response(bytes, "restart"),
            TelescopeCommand::GetDirection => parse_direction_response(bytes, "get direction"),
            TelescopeCommand::SetDirection(_) => parse_direction_response(bytes, "set direction"),
        }
    }
}

fn parse_ack_response(
    bytes: &[u8],
    command_name: &str,
) -> Result<TelescopeResponse, TelescopeError> {
    if bytes.len() == 12 && bytes[0] == 0x57 && bytes[11] == 0x20 {
        Ok(TelescopeResponse::Ack)
    } else {
        Err(TelescopeError::TelescopeIOError(format!(
            "Unexpected response to {} command: {:?}",
            command_name, bytes,
        )))
    }
}

fn parse_direction_response(
    bytes: &[u8],
    command_name: &str,
) -> Result<TelescopeResponse, TelescopeError> {
    if bytes.len() == 12 && bytes[0] == 0x58 && bytes[11] == 0x20 {
        let azimuth = rot2prog_bytes_to_angle(&bytes[1..=5]);
        let elevation = rot2prog_bytes_to_angle(&bytes[6..=10]);
        Ok(TelescopeResponse::CurrentDirection(Direction {
            azimuth,
            elevation,
        }))
    } else {
        Err(TelescopeError::TelescopeIOError(format!(
            "Unexpected response to {} command: {:?}",
            command_name, bytes,
        )))
    }
}

fn create_connection(address: &str) -> Result<TcpStream, std::io::Error> {
    // FIXME: How to handle static configuration like timeouts etc?
    let timeout = Duration::from_secs(1);
    let address = SocketAddr::from_str(address).unwrap();
    let stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

fn rot2prog_bytes_to_int(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .rev()
        .enumerate()
        .map(|(pos, &digit)| digit as u32 * 10_u32.pow(pos as u32))
        .sum()
}

fn rot2prog_bytes_to_angle(bytes: &[u8]) -> f64 {
    (rot2prog_bytes_to_int(bytes) as f64 / 100.0 - 360.0).to_radians()
}

// Responses are documented as ascii encoded numbers, but the telescope seems to return the
// bytes directly.
fn rot2prog_angle_to_bytes(angle: f64) -> [u8; 5] {
    let mut bytes = [0; 5];
    let angle = ((angle.to_degrees() + 360.0) * 100.0).round();
    bytes[0] = (angle / 10000.0) as u8 + 0x30;
    bytes[1] = ((angle % 10000.0) / 1000.0) as u8 + 0x30;
    bytes[2] = ((angle % 1000.0) / 100.0) as u8 + 0x30;
    bytes[3] = ((angle % 100.0) / 10.0) as u8 + 0x30;
    bytes[4] = (angle % 10.0) as u8 + 0x30;
    bytes
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_ack_response() {
        let res = parse_ack_response(&hex!("570000000000000000000020"), "test").unwrap();
        assert_eq!(res, TelescopeResponse::Ack);
        let res = parse_ack_response(&hex!("560000000000000000000020"), "test");
        assert_eq!(
            res,
            Err(TelescopeError::TelescopeIOError(
                "Unexpected response to test command: [86, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32]"
                    .to_string()
            ))
        );
        let res = parse_ack_response(&hex!("5700000000000000000020"), "test");
        assert_eq!(
            res,
            Err(TelescopeError::TelescopeIOError(
                "Unexpected response to test command: [87, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32]"
                    .to_string()
            ))
        );
    }

    #[test]
    fn test_parse_direction_response() {
        let res =
            parse_direction_response(&hex!("58 03 06 00 00 00 03 06 00 00 00 20"), "test").unwrap();
        assert_eq!(
            res,
            TelescopeResponse::CurrentDirection(Direction {
                azimuth: 0.0,
                elevation: 0.0,
            })
        );
    }
    #[test]
    fn test_rot2prog_bytes_to_int() {
        assert_eq!(rot2prog_bytes_to_int(&hex!("00")), 0);
        assert_eq!(rot2prog_bytes_to_int(&hex!("01")), 1);
        assert_eq!(rot2prog_bytes_to_int(&hex!("00 01")), 1);
        assert_eq!(rot2prog_bytes_to_int(&hex!("01 02")), 12);
        assert_eq!(rot2prog_bytes_to_int(&hex!("09 09 09")), 999);
    }

    #[test]
    fn test_rot2prog_angle_to_bytes() {
        assert_eq!(
            rot2prog_angle_to_bytes(0.0),
            hex!("3336303030"),
            "0.0 should be 0x3336303030 (telescope expects angle + 360)"
        );
        assert_eq!(
            rot2prog_angle_to_bytes(5.54_f64.to_radians()),
            hex!("3336353534"),
            "5.54 should be 0x3336353534 (example from documentation)"
        );
    }

    #[test]
    fn test_rot2prog_bytes_to_angle() {
        assert!((rot2prog_bytes_to_angle(&hex!("0306000000")) - 0.0).abs() < 0.01,);
    }
}
