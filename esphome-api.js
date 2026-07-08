'use strict';

const net = require('net');

// ---- ESPHome API message type IDs (verified from live HA traffic) ----
const T = {
  HelloRequest: 1,      HelloResponse: 2,
  ConnectRequest: 3,    ConnectResponse: 4,
  DisconnectRequest: 5, DisconnectResponse: 6,
  PingRequest: 7,       PingResponse: 8,
  GetTimeRequest: 9,    GetTimeResponse: 10,
  ListEntitiesRequest: 11,
  ListEntitiesSwitchResponse: 17,
  ListEntitiesDoneResponse: 19,
  SubscribeStatesRequest: 20,
  SwitchStateResponse: 26,
  SwitchCommandRequest: 33,
};

// Key for camera index → stable fixed32
function camKey(index) { return (0xCA00 + index + 1) >>> 0; }

function slugify(name) {
  return (name || '').toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_|_$/g, '') || 'camera';
}

// ---- Protobuf helpers ----

function varint(n) {
  n = n >>> 0;
  const b = [];
  while (n > 127) { b.push((n & 0x7F) | 0x80); n >>>= 7; }
  b.push(n);
  return Buffer.from(b);
}

function pbStr(fnum, str) {
  const s = Buffer.from(str || '', 'utf8');
  return Buffer.concat([varint((fnum << 3) | 2), varint(s.length), s]);
}

function pbBool(fnum, v) {
  return Buffer.concat([varint((fnum << 3) | 0), varint(v ? 1 : 0)]);
}

function pbFixed32(fnum, n) {
  const tag = varint((fnum << 3) | 5);
  const val = Buffer.alloc(4);
  val.writeUInt32LE(n >>> 0);
  return Buffer.concat([tag, val]);
}

function pbVarint(fnum, n) {
  return Buffer.concat([varint((fnum << 3) | 0), varint(n)]);
}

// ---- Frame encoding: 0x00 | varint(payload_len) | varint(msgType) | payload ----
function frame(msgType, payload) {
  return Buffer.concat([Buffer.from([0x00]), varint(payload.length), varint(msgType), payload]);
}

// ---- Frame decoding ----
function readVarint(buf, pos) {
  let v = 0, shift = 0;
  while (pos < buf.length) {
    const b = buf[pos++];
    v |= (b & 0x7F) << shift;
    shift += 7;
    if (!(b & 0x80)) break;
  }
  return { v, pos };
}

function decodeFrames(buf) {
  const frames = [];
  let pos = 0;
  while (pos < buf.length) {
    const start = pos;
    if (buf[pos] !== 0x00) { pos++; continue; }
    pos++;
    if (pos >= buf.length) { pos = start; break; }
    const lenR = readVarint(buf, pos);
    pos = lenR.pos;
    const payloadLen = lenR.v;
    if (pos >= buf.length) { pos = start; break; }
    const typeR = readVarint(buf, pos);
    const payloadStart = typeR.pos;
    if (payloadStart + payloadLen > buf.length) { pos = start; break; }
    frames.push({ type: typeR.v, payload: buf.slice(payloadStart, payloadStart + payloadLen) });
    pos = payloadStart + payloadLen;
  }
  return { frames, remaining: buf.slice(pos) };
}

// Parse SwitchCommandRequest → { key, state }
function parseSwitchCommand(payload) {
  let pos = 0, key = 0, state = false;
  while (pos < payload.length) {
    const t = readVarint(payload, pos); pos = t.pos;
    const field = t.v >> 3, wire = t.v & 7;
    if (field === 1 && wire === 5) {
      key = payload.readUInt32LE(pos); pos += 4;
    } else if (field === 2 && wire === 0) {
      const v = readVarint(payload, pos); pos = v.pos; state = v.v !== 0;
    } else if (wire === 5) { pos += 4; }
      else if (wire === 0) { const v = readVarint(payload, pos); pos = v.pos; }
      else if (wire === 2) { const v = readVarint(payload, pos); pos = v.pos + v.v; }
  }
  return { key, state };
}

// ---- ESPHomeServer ----

class ESPHomeServer {
  constructor({ port = 6053, deviceName = 'HA Camera Viewer', getCameras, onShowCamera, onHideCamera }) {
    this.port         = port;
    this.deviceName   = deviceName;
    this.getCameras   = getCameras;    // () => [{ name, entityId }]
    this.onShowCamera = onShowCamera;  // (camera) => void
    this.onHideCamera = onHideCamera;  // () => void
    this.server       = null;
    this.clients      = new Set();

    this.activeIndex  = -1;  // which camera is currently showing (-1 = none)
  }

  start() {
    this.server = net.createServer(sock => this._onClient(sock));
    this.server.on('error', e => console.error('ESPHome TCP error:', e.message));
    this.server.listen(this.port, '0.0.0.0', () =>
      console.log(`ESPHome API listening on :${this.port}`)
    );
  }

  stop() {
    for (const c of this.clients) try { c.destroy(); } catch {}
    if (this.server) this.server.close();
  }

  // Called from main.js when a camera is shown or hidden externally
  setActiveCamera(index) {
    this.activeIndex = index;
    this._broadcastStates();
  }

  _broadcastStates() {
    for (const c of this.clients) this._sendStates(c);
  }

  _sendStates(sock) {
    try {
      const cameras = this.getCameras();
      cameras.forEach((_, i) => {
        sock.write(frame(T.SwitchStateResponse, Buffer.concat([
          pbFixed32(1, camKey(i)),
          pbBool(2, this.activeIndex === i),
        ])));
      });
    } catch {}
  }

  _onClient(sock) {
    this.clients.add(sock);
    let buf = Buffer.alloc(0);
    console.log('[ESPHome] Client connected from', sock.remoteAddress);

    sock.on('data', chunk => {
      buf = Buffer.concat([buf, chunk]);
      const { frames, remaining } = decodeFrames(buf);
      buf = remaining;
      for (const f of frames) this._onMessage(sock, f.type, f.payload);
    });

    sock.on('close', () => { console.log('[ESPHome] Disconnected'); this.clients.delete(sock); });
    sock.on('error', () => { this.clients.delete(sock); try { sock.destroy(); } catch {} });
  }

  _onMessage(sock, type, payload) {
    switch (type) {

      case T.HelloRequest:
        sock.write(frame(T.HelloResponse, Buffer.concat([
          pbVarint(2, 1),
          pbVarint(3, 10),
          pbStr(4, 'ha-camera-viewer 1.0.0'),
          pbStr(5, this.deviceName),
        ])));
        break;

      case T.ConnectRequest:
        sock.write(frame(T.ConnectResponse, pbBool(1, false)));
        break;

      case T.PingRequest:
        sock.write(frame(T.PingResponse, Buffer.alloc(0)));
        break;

      case T.GetTimeRequest:
        sock.write(frame(T.GetTimeResponse,
          pbVarint(1, Math.floor(Date.now() / 1000))
        ));
        break;

      case T.DisconnectRequest:
        sock.write(frame(T.DisconnectResponse, Buffer.alloc(0)));
        sock.destroy();
        break;

      case T.ListEntitiesRequest: {
        const cameras = this.getCameras();
        cameras.forEach((cam, i) => {
          const slug = slugify(cam.name);
          sock.write(frame(T.ListEntitiesSwitchResponse, Buffer.concat([
            pbStr(1, `cam_${slug}`),
            pbFixed32(2, camKey(i)),
            pbStr(3, cam.name),
            pbStr(4, `ha_cam_viewer_${slug}`),
            pbStr(5, 'mdi:cctv'),
            pbBool(6, false),
          ])));
        });
        sock.write(frame(T.ListEntitiesDoneResponse, Buffer.alloc(0)));
        break;
      }

      case T.SubscribeStatesRequest:
        this._sendStates(sock);
        break;

      case T.SwitchCommandRequest: {
        const { key, state } = parseSwitchCommand(payload);
        const cameras = this.getCameras();
        const idx = cameras.findIndex((_, i) => camKey(i) === key);
        if (idx === -1) break;

        if (state) {
          this.activeIndex = idx;
          this._broadcastStates();
          if (this.onShowCamera) this.onShowCamera(cameras[idx]);
        } else {
          this.activeIndex = -1;
          this._broadcastStates();
          if (this.onHideCamera) this.onHideCamera();
        }
        break;
      }

      default: break;
    }
  }
}

module.exports = { ESPHomeServer };
