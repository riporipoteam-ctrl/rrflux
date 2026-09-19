// RRFlux API function — one Express app serving:
//   /api/*  -> the patched 2022 game client (Rec Room-compatible surface)
//   /v1/*   -> the RRFlux launcher (manifest, version)
//
// v1 implements only what the client needs at login/lobby. Everything else
// returns a graceful JSON 404 — the client tolerates missing endpoints.
// Response shapes are best-effort until we capture real client traffic.

const { onRequest } = require("firebase-functions/v2/https");
const admin = require("firebase-admin");
const express = require("express");

admin.initializeApp();
const db = admin.firestore();

const app = express();
app.use(express.json());

// --- auth middleware: Firebase ID token -> req.uid (optional) ---
app.use(async (req, res, next) => {
  const h = req.get("Authorization") || "";
  const m = h.match(/^Bearer (.+)$/);
  if (m) {
    try {
      req.uid = (await admin.auth().verifyIdToken(m[1])).uid;
    } catch { /* unauthenticated, continue */ }
  }
  next();
});

// ================= game client: /api/* =================

// Client version check — tell the client it's up to date.
app.get("/api/versioncheck/v4", (req, res) => {
  res.json({ ok: true, updateRequired: false });
});
app.get("/api/versioncheck/*", (req, res) => {
  res.json({ ok: true, updateRequired: false });
});

// Remote config — permissive defaults; unknown flags off.
app.get("/api/config/*", (req, res) => res.json({}));

// Player profile
app.get("/api/players/v2/me", async (req, res) => {
  if (!req.uid) return res.status(401).json({ error: "auth required" });
  const snap = await db.collection("players").doc(req.uid).get();
  if (!snap.exists) return res.status(404).json({ error: "no profile" });
  res.json({ playerId: req.uid, ...snap.data() });
});
app.get("/api/players/v2/:id", async (req, res) => {
  const snap = await db.collection("players").doc(req.params.id).get();
  if (!snap.exists) return res.status(404).json({ error: "unknown player" });
  const d = snap.data();
  res.json({ playerId: req.params.id, username: d.username });
});

// Text sanitization — private server, pass through.
app.post("/api/sanitize/*", (req, res) => {
  res.json({ text: (req.body && req.body.text) || "" });
});
app.get("/api/sanitize/*", (req, res) => {
  res.json({ text: req.query.text || "" });
});

// Graceful 404 for the rest of the game surface (friends, rooms, store,
// images, inventions, avatar, reports, reputation, messages, roomkeys…).
// We implement these as the client proves it needs them.
app.use("/api/", (req, res) => {
  console.log("unimplemented game endpoint:", req.method, req.path);
  res.status(404).json({ error: "not implemented in RRFlux v1" });
});

// ================= launcher: /v1/* =================

app.get("/v1/manifest/version", async (req, res) => {
  const snap = await db.collection("meta").doc("manifest").get();
  res.json({ version: (snap.data() || {}).version || "0.0.0" });
});

app.get("/v1/manifest", async (req, res) => {
  const snap = await db.collection("meta").doc("manifest").get();
  if (!snap.exists) return res.status(404).json({ error: "no manifest published" });
  res.json(snap.data());
});

app.get("/v1/profile/me", async (req, res) => {
  if (!req.uid) return res.status(401).json({ error: "auth required" });
  const snap = await db.collection("players").doc(req.uid).get();
  res.json({ playerId: req.uid, ...(snap.data() || {}) });
});

app.use("/v1/", (req, res) => res.status(404).json({ error: "unknown endpoint" }));

exports.api = onRequest({ cors: true }, app);
