// Logging MQTT broker used to capture how the Aquilo sensor talks to
// mqtt.aquilo.cloud. Accepts ANY client (no auth, any cert) and logs every
// CONNECT / credential / SUBSCRIBE / PUBLISH so we can reverse-engineer the
// topics and payloads. Listens plaintext on 1883 and TLS on 8883.
//
// Usage:
//   npm install
//   node broker.js
// Then in AdGuard add a DNS rewrite: mqtt.aquilo.cloud -> 172.20.0.146
//
// What to watch for:
//   - which transport the device uses (1883 plain vs 8883 TLS)
//   - on 8883: a TLS error means the device validates the LE server cert
//   - the topics it PUBLISHes (telemetry up) and SUBSCRIBEs to (state/computed
//     values coming back down) -> this is the protocol we must reimplement.

const fs = require("fs");
const net = require("net");
const tls = require("tls");
const path = require("path");
const aedes = require("aedes")();

function ts() {
  return new Date().toISOString().slice(11, 23);
}
function log(...a) {
  console.log(ts(), ...a);
}

function dump(label, topic, payload) {
  const buf = Buffer.isBuffer(payload) ? payload : Buffer.from(payload || "");
  const text = buf.toString("utf8");
  const printable = /^[\x09\x0a\x0d\x20-\x7e]*$/.test(text);
  log(`${label} topic="${topic}" (${buf.length}b)`);
  if (printable && buf.length) log("   text:", text);
  else if (buf.length) log("   hex :", buf.toString("hex"));
}

// Accept every client, but log the credentials it offered.
aedes.authenticate = (client, username, password, cb) => {
  log(
    `AUTH client="${client ? client.id : "?"}" user=${
      username ? JSON.stringify(username.toString()) : "<none>"
    } pass=${password ? JSON.stringify(password.toString()) : "<none>"}`,
  );
  cb(null, true);
};

aedes.on("client", (c) =>
  log(`CONNECT client="${c.id}" from ${c.conn && c.conn.remoteAddress}`),
);
aedes.on("clientDisconnect", (c) => log(`DISCONNECT client="${c.id}"`));
aedes.on("subscribe", (subs, c) =>
  log(
    `SUBSCRIBE client="${c && c.id}" topics=${JSON.stringify(subs.map((s) => s.topic))}`,
  ),
);
aedes.on("unsubscribe", (subs, c) =>
  log(`UNSUBSCRIBE client="${c && c.id}" topics=${JSON.stringify(subs)}`),
);
aedes.on("publish", (packet, client) => {
  if (!client) return; // skip broker's own $SYS messages
  dump("PUBLISH", packet.topic, packet.payload);
});
aedes.on("clientError", (c, e) =>
  log(`clientError client="${c && c.id}": ${e.message}`),
);
aedes.on("connectionError", (c, e) => log(`connectionError: ${e.message}`));

// Plaintext MQTT :1883
net
  .createServer(aedes.handle)
  .listen(1883, () => log("listening MQTT      on :1883"));

// MQTT over TLS :8883 (self-signed; a TLS error here = device validates the cert)
try {
  const opts = {
    key: fs.readFileSync(path.join(__dirname, "key.pem")),
    cert: fs.readFileSync(path.join(__dirname, "cert.pem")),
    requestCert: true,
    rejectUnauthorized: false,
  };
  const tlsServer = tls.createServer(opts, aedes.handle);
  tlsServer.on("tlsClientError", (e) => log(`tlsClientError: ${e.message}`));
  tlsServer.listen(8883, () => log("listening MQTT/TLS  on :8883"));
} catch (e) {
  log(
    "TLS listener not started (need cert.pem/key.pem — run gen-cert.sh):",
    e.message,
  );
}
