#!/usr/bin/env python3
"""Logging TLS server used to fingerprint how the Aquilo sensor talks to its
AWS backend.

Point one of the device's cloud domains at this host via an AdGuard DNS rewrite,
then watch the log. For every connection we record:

  * the SNI hostname from the ClientHello (what the device asked for)
  * whether the device presented a CLIENT certificate (mutual TLS) and its subject
  * whether the TLS handshake COMPLETED:
      - completes  -> device does NOT validate our (self-signed) server cert,
                      so we can impersonate the endpoint locally. We then log the
                      first request bytes (HTTP request / MQTT CONNECT packet).
      - fails with an alert -> device validates the server cert chain. Pure DNS
                      redirection won't work; firmware-level work would be needed.

Listens on multiple ports so a single run covers HTTPS (443) and MQTT/TLS (8883).
Bind to <1024 requires privilege: run with sudo.
"""

import argparse
import datetime
import socket
import ssl
import threading

LOG_LOCK = threading.Lock()


def log(*parts: object) -> None:
    ts = datetime.datetime.now().strftime("%H:%M:%S.%f")[:-3]
    with LOG_LOCK:
        print(ts, *parts, flush=True)


def make_context(cert: str, key: str) -> ssl.SSLContext:
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(certfile=cert, keyfile=key)
    # Request the client cert but don't verify it — we just want to SEE it.
    ctx.verify_mode = ssl.CERT_OPTIONAL
    ctx.check_hostname = False
    # Capture the SNI as soon as the ClientHello arrives (fires even if the
    # handshake later aborts during cert validation).
    ctx.sni_callback = lambda sslobj, name, _ctx: log("    SNI:", name)
    return ctx


def handle(raw: socket.socket, addr, ctx: ssl.SSLContext, port: int) -> None:
    peer = f"{addr[0]}:{addr[1]}"
    log(f"[:{port}] connection from {peer}")
    try:
        tls = ctx.wrap_socket(raw, server_side=True)
    except ssl.SSLError as e:
        # This is the key signal for server-cert validation: the device aborted.
        log(f"[:{port}] {peer} TLS handshake FAILED -> {e}")
        log(f"[:{port}] {peer} => device likely VALIDATES the server cert")
        raw.close()
        return
    except OSError as e:
        log(f"[:{port}] {peer} socket error during handshake: {e}")
        raw.close()
        return

    log(f"[:{port}] {peer} TLS handshake OK (cipher={tls.cipher()[0]})")
    log(f"[:{port}] {peer} => device does NOT validate our server cert")

    client_cert = tls.getpeercert(binary_form=True)
    if client_cert:
        log(f"[:{port}] {peer} client presented a cert ({len(client_cert)} bytes, mTLS)")
        try:
            der = tls.getpeercert()  # empty when we don't verify; binary form above is what matters
            if der:
                log(f"[:{port}] {peer} client cert subject: {der.get('subject')}")
        except Exception:
            pass
    else:
        log(f"[:{port}] {peer} no client cert presented")

    try:
        tls.settimeout(8)
        data = tls.recv(8192)
        if data:
            log(f"[:{port}] {peer} first {len(data)} bytes of request:")
            # Show as text when printable (HTTP), else hex (MQTT/binary).
            try:
                text = data.decode("utf-8")
                if text.isprintable() or "\n" in text or "\r" in text:
                    for line in text.splitlines():
                        log("      |", line)
                else:
                    raise ValueError
            except (UnicodeDecodeError, ValueError):
                log("      hex:", data.hex())
        else:
            log(f"[:{port}] {peer} no application data before close")
    except socket.timeout:
        log(f"[:{port}] {peer} no data within timeout")
    except OSError as e:
        log(f"[:{port}] {peer} read error: {e}")
    finally:
        try:
            tls.close()
        except OSError:
            pass


def serve(port: int, ctx: ssl.SSLContext) -> None:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", port))
    srv.listen(16)
    log(f"listening on :{port}")
    while True:
        raw, addr = srv.accept()
        threading.Thread(target=handle, args=(raw, addr, ctx, port), daemon=True).start()


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cert", default="cert.pem")
    ap.add_argument("--key", default="key.pem")
    ap.add_argument("--ports", default="443,8883", help="comma-separated TLS ports")
    args = ap.parse_args()

    ctx = make_context(args.cert, args.key)
    ports = [int(p) for p in args.ports.split(",")]
    for p in ports[:-1]:
        threading.Thread(target=serve, args=(p, ctx), daemon=True).start()
    serve(ports[-1], ctx)


if __name__ == "__main__":
    main()
