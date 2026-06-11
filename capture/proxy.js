// Transparent MQTT proxy: device -> here -> real mqtt.aquilo.cloud, logging both
// directions. This captures what the REAL server sends back (state, params, ping,
// version) — the payloads we must reimplement to run offline.
//
// Keep the AdGuard rewrite mqtt.aquilo.cloud -> 172.20.0.146 in place. We connect
// upstream by IP so we don't loop back into ourselves.
//
//   node proxy.js
//
// Bytes are forwarded verbatim; we only TEE a copy through an MQTT decoder for
// logging, so the protocol is never altered.

const net = require("net");

const UPSTREAM_HOST = process.env.UPSTREAM_HOST || "57.128.198.238"; // mqtt.aquilo.cloud (by IP, on purpose)
const UPSTREAM_PORT = Number(process.env.UPSTREAM_PORT || 1883);
const LISTEN_PORT = Number(process.env.LISTEN_PORT || 1883);

const TYPES = {
  1: "CONNECT",
  2: "CONNACK",
  3: "PUBLISH",
  4: "PUBACK",
  5: "PUBREC",
  6: "PUBREL",
  7: "PUBCOMP",
  8: "SUBSCRIBE",
  9: "SUBACK",
  10: "UNSUBSCRIBE",
  11: "UNSUBACK",
  12: "PINGREQ",
  13: "PINGRESP",
  14: "DISCONNECT",
};

function ts() {
  return new Date().toISOString().slice(11, 23);
}

// Decodes a byte stream of MQTT packets, logging each one. One instance per
// direction per connection (keeps its own reassembly buffer).
class MqttDecoder {
  constructor(tag) {
    this.tag = tag;
    this.buf = Buffer.alloc(0);
  }

  push(chunk) {
    this.buf = Buffer.concat([this.buf, chunk]);
    for (;;) {
      if (this.buf.length < 2) return;
      // remaining-length varint starts at byte 1
      let mult = 1,
        len = 0,
        i = 1,
        b;
      do {
        if (i >= this.buf.length) return; // need more bytes for the length field
        b = this.buf[i++];
        len += (b & 0x7f) * mult;
        mult *= 128;
      } while (b & 0x80);
      const total = i + len;
      if (this.buf.length < total) return; // wait for the full packet
      this.handle(this.buf.subarray(0, total), i, len);
      this.buf = this.buf.subarray(total);
    }
  }

  handle(pkt, headerEnd, remLen) {
    const type = pkt[0] >> 4;
    const flags = pkt[0] & 0x0f;
    const name = TYPES[type] || `?${type}`;
    const body = pkt.subarray(headerEnd, headerEnd + remLen);

    if (type === 3) {
      // PUBLISH: topic, optional packetId (QoS>0), payload
      const tlen = body.readUInt16BE(0);
      const topic = body.subarray(2, 2 + tlen).toString("utf8");
      const qos = (flags >> 1) & 0x03;
      let off = 2 + tlen + (qos > 0 ? 2 : 0);
      const payload = body.subarray(off);
      const text = payload.toString("utf8");
      const printable = /^[\x09\x0a\x0d\x20-\x7e]*$/.test(text);
      console.log(
        `${ts()} ${this.tag} PUBLISH q${qos}${flags & 8 ? " dup" : ""}${flags & 1 ? " retain" : ""} topic="${topic}" (${payload.length}b)`,
      );
      if (payload.length)
        console.log(
          `        ${printable ? "text: " + text : "hex : " + payload.toString("hex")}`,
        );
    } else if (type === 8 || type === 10) {
      // (UN)SUBSCRIBE: skip 2-byte packetId, then topic list
      const topics = [];
      let off = 2;
      while (off < body.length) {
        const l = body.readUInt16BE(off);
        off += 2;
        topics.push(body.subarray(off, off + l).toString("utf8"));
        off += l + (type === 8 ? 1 : 0); // SUBSCRIBE has a QoS byte per topic
      }
      console.log(`${ts()} ${this.tag} ${name} ${JSON.stringify(topics)}`);
    } else if (type === 1) {
      console.log(`${ts()} ${this.tag} CONNECT (${remLen}b)`);
    } else {
      console.log(`${ts()} ${this.tag} ${name}`);
    }
  }
}

let n = 0;
net
  .createServer((client) => {
    const id = ++n;
    const peer = client.remoteAddress;
    console.log(
      `${ts()} === conn#${id} from ${peer} -> ${UPSTREAM_HOST}:${UPSTREAM_PORT} ===`,
    );
    const upstream = net.connect(UPSTREAM_PORT, UPSTREAM_HOST);
    const cDec = new MqttDecoder(`#${id} C->S`);
    const sDec = new MqttDecoder(`#${id} S->C`);

    client.on("data", (d) => {
      try {
        cDec.push(d);
      } catch (e) {
        console.log("decode C->S err", e.message);
      }
      upstream.write(d);
    });
    upstream.on("data", (d) => {
      try {
        sDec.push(d);
      } catch (e) {
        console.log("decode S->C err", e.message);
      }
      client.write(d);
    });

    const close = (who) => () => {
      console.log(`${ts()} === conn#${id} closed (${who}) ===`);
      client.destroy();
      upstream.destroy();
    };
    client.on("close", close("client"));
    upstream.on("close", close("upstream"));
    client.on("error", (e) => console.log(`#${id} client err: ${e.message}`));
    upstream.on("error", (e) =>
      console.log(`#${id} upstream err: ${e.message}`),
    );
  })
  .listen(LISTEN_PORT, () =>
    console.log(
      `${ts()} MQTT proxy listening :${LISTEN_PORT} -> ${UPSTREAM_HOST}:${UPSTREAM_PORT}`,
    ),
  );
