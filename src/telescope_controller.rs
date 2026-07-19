use crate::coords::Direction;
use crate::models::telescope_types::TelescopeError;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::str::FromStr;
use std::time::Duration;

/// Timeout for connecting to and talking with the ROT2PROG rotor
/// controller. It sits on the local network, so one second is generous;
/// promote to config if a deployment ever needs a different value.
const CONTROLLER_IO_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TelescopeCommand {
    Stop,
    Restart,
    GetDirection,
    SetDirection(Direction),
    /// Overwrite the controller's stored current position without moving
    /// the rotor (ROTn_CMD_CALIBRATION). Used to correct pointing offsets.
    Calibrate(Direction),
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TelescopeResponse {
    Ack,
    CurrentDirection(Direction),
}

pub struct TelescopeController {
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
            TelescopeCommand::Stop => [
                0x57, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0F, 0x20,
            ]
            .into(),
            TelescopeCommand::Restart => [
                0x57, 0xEF, 0xBE, 0xAD, 0xDE, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xEE, 0x20,
            ]
            .into(),
            TelescopeCommand::GetDirection => [
                0x57, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6F, 0x20,
            ]
            .into(),
            TelescopeCommand::SetDirection(direction) => {
                let mut bytes = Vec::with_capacity(13);
                bytes.extend([0x57]);
                bytes.extend(rot2prog_angle_to_bytes(direction.azimuth).as_slice());
                bytes.extend(rot2prog_angle_to_bytes(direction.elevation).as_slice());
                bytes.extend([0x5F, 0x20]);
                bytes
            }
            TelescopeCommand::Calibrate(direction) => {
                let mut bytes = Vec::with_capacity(13);
                bytes.extend([0x57]);
                bytes.extend(rot2prog_calibration_angle_to_bytes(direction.azimuth).as_slice());
                bytes.extend(rot2prog_calibration_angle_to_bytes(direction.elevation).as_slice());
                bytes.extend([0xF9, 0x20]);
                bytes
            }
        }
    }

    fn parse_response(&self, bytes: &[u8]) -> Result<TelescopeResponse, TelescopeError> {
        match self {
            // Stop returns a direction response (0x58) when idle, or an ACK
            // (0x57) when actively stopping a moving rotor.
            TelescopeCommand::Stop => parse_direction_response(bytes, "stop")
                .or_else(|_| parse_ack_response(bytes, "stop")),
            TelescopeCommand::Restart => parse_ack_response(bytes, "restart"),
            TelescopeCommand::GetDirection => parse_direction_response(bytes, "get direction"),
            TelescopeCommand::SetDirection(_) => parse_direction_response(bytes, "set direction"),
            TelescopeCommand::Calibrate(_) => parse_legacy_direction_response(bytes, "calibrate"),
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

// The calibration command exists only in the legacy protocol format, where
// each angle is four digit characters scaled by a divisor byte. Four digits
// cannot hold (angle + 360) × 100, so 10 units per degree is effectively the
// maximum — which matches the rotor's 0.1° mechanical resolution anyway.
const CALIBRATION_DIVISOR: u8 = 10;

/// Parse the legacy 12-byte position frame returned by the calibration
/// command: start byte 0x57, four raw digit bytes + divisor per angle,
/// angle = value / divisor − 360.
fn parse_legacy_direction_response(
    bytes: &[u8],
    command_name: &str,
) -> Result<TelescopeResponse, TelescopeError> {
    if bytes.len() == 12 && bytes[0] == 0x57 && bytes[11] == 0x20 && bytes[5] != 0 && bytes[10] != 0
    {
        let azimuth = rot2prog_legacy_bytes_to_angle(&bytes[1..=4], bytes[5]);
        let elevation = rot2prog_legacy_bytes_to_angle(&bytes[6..=9], bytes[10]);
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

fn create_connection(address: &str) -> Result<TcpStream, TelescopeError> {
    let timeout = CONTROLLER_IO_TIMEOUT;
    let address = SocketAddr::from_str(address).map_err(|err| {
        TelescopeError::TelescopeIOError(format!(
            "invalid controller address '{address}' in config: {err}"
        ))
    })?;
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

fn rot2prog_legacy_bytes_to_angle(digits: &[u8], divisor: u8) -> f64 {
    (rot2prog_bytes_to_int(digits) as f64 / f64::from(divisor) - 360.0).to_radians()
}

/// Legacy-format angle field: four ASCII digits of
/// (angle_degrees + 360) × divisor, followed by the divisor byte.
fn rot2prog_calibration_angle_to_bytes(angle: f64) -> [u8; 5] {
    let mut bytes = [0; 5];
    let value = ((angle.to_degrees() + 360.0) * f64::from(CALIBRATION_DIVISOR)).round();
    bytes[0] = (value / 1000.0) as u8 + 0x30;
    bytes[1] = ((value % 1000.0) / 100.0) as u8 + 0x30;
    bytes[2] = ((value % 100.0) / 10.0) as u8 + 0x30;
    bytes[3] = (value % 10.0) as u8 + 0x30;
    bytes[4] = CALIBRATION_DIVISOR;
    bytes
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
        let res = parse_ack_response(
            &[
                0x57, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20,
            ],
            "test",
        )
        .unwrap();
        assert_eq!(res, TelescopeResponse::Ack);
        let res = parse_ack_response(
            &[
                0x56, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20,
            ],
            "test",
        );
        assert_eq!(
            res,
            Err(TelescopeError::TelescopeIOError(
                "Unexpected response to test command: [86, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32]"
                    .to_string()
            ))
        );
        let res = parse_ack_response(
            &[
                0x57, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20,
            ],
            "test",
        );
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
        let res = parse_direction_response(
            &[
                0x58, 0x03, 0x06, 0x00, 0x00, 0x00, 0x03, 0x06, 0x00, 0x00, 0x00, 0x20,
            ],
            "test",
        )
        .unwrap();
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
        assert_eq!(rot2prog_bytes_to_int(&[0x00]), 0);
        assert_eq!(rot2prog_bytes_to_int(&[0x01]), 1);
        assert_eq!(rot2prog_bytes_to_int(&[0x00, 0x01]), 1);
        assert_eq!(rot2prog_bytes_to_int(&[0x01, 0x02]), 12);
        assert_eq!(rot2prog_bytes_to_int(&[0x09, 0x09, 0x09]), 999);
    }

    #[test]
    fn test_rot2prog_angle_to_bytes() {
        assert_eq!(
            rot2prog_angle_to_bytes(0.0),
            [0x33, 0x36, 0x30, 0x30, 0x30,],
            "0.0 should be 0x3336303030 (telescope expects angle + 360)"
        );
        assert_eq!(
            rot2prog_angle_to_bytes(5.54_f64.to_radians()),
            [0x33, 0x36, 0x35, 0x35, 0x34],
            "5.54 should be 0x3336353534 (example from documentation)"
        );
    }

    #[test]
    fn test_rot2prog_bytes_to_angle() {
        assert!((rot2prog_bytes_to_angle(&[0x03, 0x06, 0x00, 0x00, 0x00,]) - 0.0).abs() < 0.01,);
    }

    #[test]
    fn test_calibrate_to_bytes_matches_official_example() {
        // "Set Motor 1 to 1 degree and Motor 2 to -1 degree" from the
        // official protocol documentation (assets/Rot2Prog_protocol_version_2.0.pdf).
        let command = TelescopeCommand::Calibrate(Direction {
            azimuth: 1.0_f64.to_radians(),
            elevation: (-1.0_f64).to_radians(),
        });
        assert_eq!(
            command.to_bytes(),
            vec![
                0x57, 0x33, 0x36, 0x31, 0x30, 0x0A, 0x33, 0x35, 0x39, 0x30, 0x0A, 0xF9, 0x20,
            ],
        );
    }

    #[test]
    fn test_calibrate_parses_legacy_response() {
        // Legacy position frame with raw digit bytes: az 22.3°, el 0.5°
        // (example values from the official documentation).
        let command = TelescopeCommand::Calibrate(Direction {
            azimuth: 0.0,
            elevation: 0.0,
        });
        let response = command
            .parse_response(&[
                0x57, 0x03, 0x08, 0x02, 0x03, 0x0A, 0x03, 0x06, 0x00, 0x05, 0x0A, 0x20,
            ])
            .unwrap();
        let TelescopeResponse::CurrentDirection(direction) = response else {
            panic!("Expected direction response");
        };
        assert!((direction.azimuth.to_degrees() - 22.3).abs() < 0.01);
        assert!((direction.elevation.to_degrees() - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_calibrate_rejects_wrong_frame() {
        let command = TelescopeCommand::Calibrate(Direction {
            azimuth: 0.0,
            elevation: 0.0,
        });
        // The 0x58 frame used by the _100 commands is not a valid
        // calibration response.
        assert!(
            command
                .parse_response(&[
                    0x58, 0x03, 0x08, 0x02, 0x03, 0x03, 0x03, 0x06, 0x00, 0x05, 0x02, 0x20,
                ])
                .is_err()
        );
    }
}
