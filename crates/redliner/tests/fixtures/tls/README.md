# Test TLS fixture

`self-signed.crt` / `self-signed.key` are a throwaway self-signed certificate
and its private key, used only by the HTTPS mock in `tests/e2e.rs` to test
`target.insecure_tls`. They protect nothing and are not a secret. The cert is
for `127.0.0.1` and `localhost` and expires in 2126.

Regenerate with:

```bash
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -days 36500 -subj "/CN=redliner-test" \
  -addext "subjectAltName=IP:127.0.0.1,DNS:localhost" \
  -keyout self-signed.key -out self-signed.crt
```
