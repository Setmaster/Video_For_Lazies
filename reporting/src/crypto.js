const encoder = new TextEncoder();
export function base64url(bytes) {
  return btoa(String.fromCharCode(...new Uint8Array(bytes))).replace(/=/g, '').replace(/\+/g, '-').replace(/\//g, '_');
}
export async function hmac(secret, value) {
  const key = await crypto.subtle.importKey('raw', encoder.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  return base64url(await crypto.subtle.sign('HMAC', key, encoder.encode(value)));
}
export async function session(secret, now = Date.now()) {
  const payload = `${crypto.randomUUID()}.${Math.floor(now / 1000)}`;
  return `${payload}.${await hmac(secret, `nonce:${payload}`)}`;
}
export async function verifySession(secret, token, now = Date.now()) {
  if (typeof token !== 'string' || !/^[a-f0-9-]{36}\.[0-9]{10}\.[A-Za-z0-9_-]{43}$/.test(token)) return null;
  const [id, stamp, signature] = token.split('.');
  const expected = await hmac(secret, `nonce:${id}.${stamp}`);
  let difference = 0;
  for (let i = 0; i < expected.length; i++) difference |= expected.charCodeAt(i) ^ signature.charCodeAt(i);
  const age = Math.floor(now / 1000) - Number(stamp);
  return difference === 0 && age >= 3 && age < 7 * 86400 ? { id, age } : null;
}

export async function appJwt(appId, pem, now = Date.now()) {
  const time = Math.floor(now / 1000);
  const header = base64url(encoder.encode(JSON.stringify({ alg: 'RS256', typ: 'JWT' })));
  const payload = base64url(encoder.encode(JSON.stringify({ iat: time - 60, exp: time + 540, iss: appId })));
  // WebCrypto imports PKCS#8. Convert GitHub's downloaded PKCS#1 key before provisioning.
  if (!pem.includes('-----BEGIN PRIVATE KEY-----')) throw new Error('PKCS#8 key required');
  const der = Uint8Array.from(atob(pem.replace(/-----[^-]+-----/g, '').replace(/\s/g, '')), c => c.charCodeAt(0));
  const key = await crypto.subtle.importKey('pkcs8', der, { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' }, false, ['sign']);
  return `${header}.${payload}.${base64url(await crypto.subtle.sign('RSASSA-PKCS1-v1_5', key, encoder.encode(`${header}.${payload}`)))}`;
}
