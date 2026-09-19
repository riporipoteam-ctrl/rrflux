// RRFlux auth function — answers the client's LoginWithToken call.
//
// The patched game calls:
//   GET /Account/LoginWithToken?loginToken={0}&accountId={1}&redirectUrl={2}
// where {0} is the Firebase ID token the launcher obtained at sign-in
// (passed to the game via command line), and {1} is the player's UID.
//
// Flow:
//   1. Verify loginToken as a Firebase ID token (Admin SDK) — the ONLY
//      accepted credential. There is no accountId fallback: trusting a
//      client-provided account ID would let anyone impersonate any player.
//   2. Ensure players/{uid} exists in Firestore (create on first login).
//   3. Mint a Firebase custom token for the client session and return it.
//
// Best-effort response shape — the exact 2022 schema is unknown until we
// capture real client traffic. The client only needs a 200 with a usable
// token; shapes are easy to tweak once we see what it parses.

const { onRequest } = require("firebase-functions/v2/https");
const admin = require("firebase-admin");

admin.initializeApp();
const db = admin.firestore();

async function ensurePlayer(uid, idToken) {
  const ref = db.collection("players").doc(uid);
  const snap = await ref.get();
  if (!snap.exists) {
    const name =
      (idToken && (idToken.name || idToken.email || "").split("@")[0]) ||
      `player-${uid.slice(0, 6)}`;
    await ref.set({
      username: name,
      createdAt: admin.firestore.FieldValue.serverTimestamp(),
      lastSeen: admin.firestore.FieldValue.serverTimestamp(),
    });
  } else {
    await ref.update({ lastSeen: admin.firestore.FieldValue.serverTimestamp() });
  }
  return (await ref.get()).data();
}

exports.loginWithToken = onRequest({ cors: true }, async (req, res) => {
  const loginToken = req.query.loginToken || (req.body && req.body.loginToken);

  if (!loginToken) {
    res.status(401).json({ error: "loginToken required" });
    return;
  }

  let decoded;
  try {
    decoded = await admin.auth().verifyIdToken(loginToken);
  } catch (e) {
    console.warn("ID token verify failed:", e.message);
    res.status(401).json({ error: "invalid loginToken" });
    return;
  }
  const uid = decoded.uid;

  try {
    const profile = await ensurePlayer(uid, decoded);
    const customToken = await admin.auth().createCustomToken(uid);
    res.json({
      playerId: uid,
      username: profile.username,
      authToken: customToken,
      expiresIn: 3600,
    });
  } catch (e) {
    console.error("login failed:", e);
    res.status(500).json({ error: "login failed" });
  }
});
