Test-only TLS certificates and keys for the Live map add-on's certificate
pinning tests (src/live.rs). Not used by the app.

- test-cert.der / test-key.der: self-signed, 127.0.0.1 (X.509 v3).
- test-cert-v1.der / test-key-v1.der: self-signed X.509 v1, like the
  dedicated server's own certificate
  (`openssl req -x509v1 -config <empty [req] config> -key ... -subj /CN=FactoryGame`).
