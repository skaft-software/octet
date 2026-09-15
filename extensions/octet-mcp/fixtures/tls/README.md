# Loopback TLS test identity

`loopback-key.pem` is a deliberately public, test-only private key. Never use it
for a real service or install its certificate in a system trust store.
The self-signed certificate covers only `fixture.invalid` and `127.0.0.1`.
The tests load it into an isolated in-memory trust context; production always
uses `ssl.create_default_context()` and never reads these fixtures.

Generated locally with OpenSSL (no external server or credential):

```sh
openssl req -x509 -newkey rsa:2048 -nodes -keyout loopback-key.pem \
  -out loopback-cert.pem -days 36500 -subj '/CN=fixture.invalid' \
  -addext 'subjectAltName=DNS:fixture.invalid,IP:127.0.0.1'
```

Assertions cover trusted success, original-hostname SNI on a numeric pin,
hostname mismatch, and an untrusted certificate; generation is not run by tests.
