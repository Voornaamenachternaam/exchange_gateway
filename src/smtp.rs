use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use url::Url;

pub struct SmtpClient {
    host: String,
    port: u16,
}

impl SmtpClient {
    pub fn from_url(smtp_url: &str) -> Result<Self> {
        let parsed = Url::parse(smtp_url)
            .map_err(|e| anyhow!("invalid SMTP URL '{}': {}", smtp_url, e))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("SMTP URL missing host"))?
            .to_string();
        let port = parsed.port().unwrap_or(25);
        Ok(SmtpClient { host, port })
    }

    pub async fn send_mime(
        &self,
        username: &str,
        password: &str,
        mime_bytes: &[u8],
    ) -> Result<()> {
        let (from_addr, recipients) = parse_envelope(mime_bytes)?;

        let stream = TcpStream::connect(format!("{}:{}", self.host, self.port))
            .await
            .map_err(|e| anyhow!("SMTP connect to {}:{} failed: {}", self.host, self.port, e))?;

        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        read_ok(&mut reader, 220).await?;

        writeln_smtp(&mut writer, "EHLO exchange-gateway").await?;
        let caps = read_multiline_ok(&mut reader, 250).await?;

        if !username.is_empty() && caps.iter().any(|l| l.to_ascii_uppercase().contains("AUTH")) {
            let plain = format!("\0{username}\0{password}");
            let b64 = BASE64.encode(plain.as_bytes());
            writeln_smtp(&mut writer, &format!("AUTH PLAIN {b64}")).await?;
            read_ok(&mut reader, 235).await?;
        }

        writeln_smtp(&mut writer, &format!("MAIL FROM:<{from_addr}>")).await?;
        read_ok(&mut reader, 250).await?;

        for rcpt in &recipients {
            writeln_smtp(&mut writer, &format!("RCPT TO:<{rcpt}>")).await?;
            read_ok(&mut reader, 250).await?;
        }

        writeln_smtp(&mut writer, "DATA").await?;
        read_ok(&mut reader, 354).await?;

        let content = String::from_utf8_lossy(mime_bytes);
        for line in content.lines() {
            if line.starts_with('.') {
                writer.write_all(b".").await?;
            }
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\r\n").await?;
        }
        writer.write_all(b".\r\n").await?;
        read_ok(&mut reader, 250).await?;

        let _ = writeln_smtp(&mut writer, "QUIT").await;

        Ok(())
    }
}

fn parse_envelope(mime_bytes: &[u8]) -> Result<(String, Vec<String>)> {
    let content = std::str::from_utf8(mime_bytes).unwrap_or("");
    let mut from = String::new();
    let mut recipients: Vec<String> = Vec::new();

    for raw_line in content.lines() {
        if raw_line.is_empty() {
            break;
        }
        let lower = raw_line.to_ascii_lowercase();
        if lower.starts_with("from:") {
            if let Some(addr) = extract_addr(&raw_line[5..]) {
                from = addr;
            }
        } else if lower.starts_with("to:") {
            recipients.extend(extract_addrs(&raw_line[3..]));
        } else if lower.starts_with("cc:") {
            recipients.extend(extract_addrs(&raw_line[3..]));
        } else if lower.starts_with("bcc:") {
            recipients.extend(extract_addrs(&raw_line[4..]));
        }
    }

    if from.is_empty() {
        return Err(anyhow!("MIME missing From header"));
    }
    if recipients.is_empty() {
        return Err(anyhow!("MIME has no recipients"));
    }

    Ok((from, recipients))
}

fn extract_addr(s: &str) -> Option<String> {
    extract_addrs(s).into_iter().next()
}

fn extract_addrs(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        let addr = if let (Some(lt), Some(gt)) = (part.rfind('<'), part.rfind('>')) {
            if lt < gt {
                &part[lt + 1..gt]
            } else {
                part
            }
        } else {
            part
        };
        let addr = addr.trim();
        if addr.contains('@') {
            out.push(addr.to_string());
        }
    }
    out
}

async fn writeln_smtp(writer: &mut tokio::net::tcp::OwnedWriteHalf, line: &str) -> Result<()> {
    writer
        .write_all(format!("{}\r\n", line).as_bytes())
        .await
        .map_err(|e| anyhow!("SMTP write error: {}", e))
}

async fn read_ok(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    expected: u16,
) -> Result<()> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| anyhow!("SMTP read error: {}", e))?;
    let code: u16 = line
        .get(..3)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("SMTP unexpected response: {}", line.trim()))?;
    while line.len() > 3 && line.as_bytes().get(3) == Some(&b'-') {
        line.clear();
        reader
            .read_line(&mut line)
            .await
            .map_err(|e| anyhow!("SMTP read error: {}", e))?;
    }
    if code != expected {
        return Err(anyhow!("SMTP expected {} got {} ({})", expected, code, line.trim()));
    }
    Ok(())
}

async fn read_multiline_ok(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    expected: u16,
) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(|e| anyhow!("SMTP read error: {}", e))?;
        let code: u16 = line
            .get(..3)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("SMTP unexpected response: {}", line.trim()))?;
        if code != expected {
            return Err(anyhow!("SMTP expected {} got {}", expected, code));
        }
        lines.push(line.trim().to_string());
        if line.as_bytes().get(3) != Some(&b'-') {
            break;
        }
    }
    Ok(lines)
}
