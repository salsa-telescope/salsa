/// Minimal FITS writer for 1D spectra stored as 3D image HDUs (NAXIS1=N, NAXIS2=1, NAXIS3=1).
/// BITPIX=-32 (IEEE 754 single-precision float, big-endian).
/// Compliant with FITS standard (NOST 100-2.0) and WCS Paper III for spectral axes.
const BLOCK_SIZE: usize = 2880;
const RECORD_SIZE: usize = 80;

pub struct SpectrumMeta<'a> {
    pub frequencies: &'a [f64],
    pub amplitudes: &'a [f64],
    pub telescope_id: &'a str,
    pub coordinate_system: &'a str,
    pub target_x: f64,
    pub target_y: f64,
    pub integration_time_secs: f64,
    pub start_time: &'a str,
    pub vlsr_correction_mps: Option<f64>,
}

fn card_logical(key: &str, val: bool, comment: &str) -> [u8; 80] {
    card_raw(
        key,
        &format!("{:>20}", if val { "T" } else { "F" }),
        comment,
    )
}

fn card_int(key: &str, val: i64, comment: &str) -> [u8; 80] {
    card_raw(key, &format!("{:>20}", val), comment)
}

fn card_float(key: &str, val: f64, comment: &str) -> [u8; 80] {
    // FITS scientific notation: right-justified, exponent with explicit sign and 2+ digits
    let s = format!("{:.8E}", val);
    let fits_val = if let Some(e) = s.find('E') {
        let mantissa = &s[..e];
        let exp: i32 = s[e + 1..].parse().unwrap_or(0);
        format!("{}E{:+03}", mantissa, exp)
    } else {
        s
    };
    card_raw(key, &format!("{:>20}", fits_val), comment)
}

fn card_str(key: &str, val: &str, comment: &str) -> [u8; 80] {
    // String values: enclosed in single quotes, padded to at least 8 chars inside
    let padded = format!("'{:<8}'", val);
    card_raw(key, &padded, comment)
}

fn card_raw(key: &str, value: &str, comment: &str) -> [u8; 80] {
    let line = if comment.is_empty() {
        format!("{:<8}= {}", key, value)
    } else {
        format!("{:<8}= {} / {}", key, value, comment)
    };
    let mut record = [b' '; RECORD_SIZE];
    let bytes = line.as_bytes();
    let len = bytes.len().min(RECORD_SIZE);
    record[..len].copy_from_slice(&bytes[..len]);
    record
}

fn card_end() -> [u8; 80] {
    let mut record = [b' '; RECORD_SIZE];
    record[..3].copy_from_slice(b"END");
    record
}

fn pad_to_block(data: &mut Vec<u8>) {
    let rem = data.len() % BLOCK_SIZE;
    if rem != 0 {
        data.resize(data.len() + (BLOCK_SIZE - rem), 0);
    }
}

/// Write a FITS file containing a 1D spectrum as a 3D image HDU (1,1,N).
pub fn write_spectrum_fits(meta: &SpectrumMeta) -> Vec<u8> {
    let nchans = meta.amplitudes.len();

    // Derive frequency axis WCS from the actual frequency array
    let crval1 = meta.frequencies.first().copied().unwrap_or(0.0);
    let cdelt1 = if nchans > 1 {
        (meta.frequencies[nchans - 1] - meta.frequencies[0]) / (nchans as f64 - 1.0)
    } else {
        1.0
    };

    // Map coordinate system to FITS CTYPE axis names
    let (ctype2, ctype3) = match meta.coordinate_system.to_lowercase().as_str() {
        "galactic" => ("GLON", "GLAT"),
        "equatorial" => ("RA", "DEC"),
        _ => ("CLON", "CLAT"),
    };

    let mut header = vec![
        card_logical("SIMPLE", true, "file conforms to FITS standard"),
        card_int("BITPIX", -32, "IEEE 754 single-precision float"),
        card_int("NAXIS", 3, "number of data axes"),
        card_int("NAXIS1", nchans as i64, "number of frequency channels"),
        card_int("NAXIS2", 1, ""),
        card_int("NAXIS3", 1, ""),
        card_logical("EXTEND", true, "FITS dataset may contain extensions"),
        card_str("ORIGIN", "SALSA", "observatory system"),
        card_str("TELESCOP", meta.telescope_id, "telescope name"),
        card_str("DATE-OBS", meta.start_time, "date of observation"),
        card_float(
            "OBSTIME",
            meta.integration_time_secs,
            "integration time (s)",
        ),
        card_str("BUNIT", "K", "antenna temperature"),
        // Spectral axis
        card_str("CTYPE1", "FREQ", "frequency axis"),
        card_float("CRPIX1", 1.0, "reference pixel (1-based)"),
        card_float("CRVAL1", crval1, "reference frequency (Hz)"),
        card_float("CDELT1", cdelt1, "frequency step (Hz)"),
        card_str("CUNIT1", "Hz", "frequency unit"),
        // Spatial axis 2
        card_str("CTYPE2", ctype2, ""),
        card_float("CRPIX2", 1.0, ""),
        card_float("CRVAL2", meta.target_x, "coordinate (deg)"),
        card_float("CDELT2", 1.0, ""),
        card_str("CUNIT2", "deg", ""),
        // Spatial axis 3
        card_str("CTYPE3", ctype3, ""),
        card_float("CRPIX3", 1.0, ""),
        card_float("CRVAL3", meta.target_y, "coordinate (deg)"),
        card_float("CDELT3", 1.0, ""),
        card_str("CUNIT3", "deg", ""),
        // HI rest frequency
        card_float("RESTFRQ", 1_420_405_751.77, "HI rest frequency (Hz)"),
    ];

    // VLSR: modern WCS (VELOSYS + SPECSYS) and legacy (VELO-LSR) for compatibility
    if let Some(vlsr_mps) = meta.vlsr_correction_mps {
        header.push(card_float(
            "VELOSYS",
            vlsr_mps,
            "LSR velocity correction (m/s)",
        ));
        header.push(card_str("SPECSYS", "LSRK", "spectral reference frame"));
        header.push(card_float(
            "VELO-LSR",
            vlsr_mps / 1000.0,
            "LSR velocity (km/s, legacy)",
        ));
    }
    header.push(card_end());

    // Serialize header, padded to block boundary
    let mut bytes: Vec<u8> = header.iter().flat_map(|r| r.iter().copied()).collect();
    pad_to_block(&mut bytes);

    // Data: amplitudes as big-endian IEEE 754 float32
    for &amp in meta.amplitudes {
        bytes.extend_from_slice(&(amp as f32).to_be_bytes());
    }
    pad_to_block(&mut bytes);

    bytes
}
