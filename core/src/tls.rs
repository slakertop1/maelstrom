use crate::types::TlsConfig;

/// Apply per-request TLS options (client cert for mTLS, custom CA, insecure) to
/// a reqwest client builder.
pub fn apply_tls(
    mut builder: reqwest::ClientBuilder,
    tls: &Option<TlsConfig>,
) -> Result<reqwest::ClientBuilder, String> {
    let Some(tls) = tls else { return Ok(builder) };

    if tls.insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }

    if let Some(ca_path) = tls.ca_cert_pem.as_deref().filter(|p| !p.trim().is_empty()) {
        let pem = std::fs::read(ca_path)
            .map_err(|e| format!("Не удалось прочитать CA-сертификат {ca_path}: {e}"))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|e| format!("Неверный CA-сертификат: {e}"))?;
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }

    let cert_path = tls.client_cert_pem.as_deref().filter(|p| !p.trim().is_empty());
    let key_path = tls.client_key_pem.as_deref().filter(|p| !p.trim().is_empty());
    match (cert_path, key_path) {
        (Some(cert), Some(key)) => {
            let mut pem = std::fs::read(cert)
                .map_err(|e| format!("Не удалось прочитать сертификат {cert}: {e}"))?;
            let key_bytes = std::fs::read(key)
                .map_err(|e| format!("Не удалось прочитать ключ {key}: {e}"))?;
            pem.push(b'\n');
            pem.extend_from_slice(&key_bytes);
            let identity = reqwest::Identity::from_pem(&pem)
                .map_err(|e| format!("Неверная пара сертификат/ключ: {e}"))?;
            builder = builder.identity(identity);
        }
        (Some(_), None) => return Err("Указан клиентский сертификат, но не указан ключ".into()),
        (None, Some(_)) => return Err("Указан ключ, но не указан клиентский сертификат".into()),
        (None, None) => {}
    }

    Ok(builder)
}
