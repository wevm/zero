/**
 * Where the GitHub login callback hands a native app its token, instead of
 * setting the web app's cookie, which a native app can't read.
 */

/** The Swift app's (apps/zbugs-swift) web authentication session callback. */
export const swiftAppRedirect = 'zbugs-swift://auth';

/**
 * The URL to send a native app's token to, with the token in its `jwt`
 * query parameter, or `undefined` when `redirect` is the web app's.
 *
 * - `zbugs-swift://auth`: the Swift app's web authentication session, which
 *   hands the callback only to the app that started it, even if another
 *   app registers the scheme. The app doesn't register it with the system,
 *   so the link isn't opened any other way.
 * - A loopback redirect (RFC 8252 §7.3), as a desktop app gets the token:
 *   `http` to `127.0.0.1` or `[::1]` with a port, which only reaches a
 *   process on the user's own machine. The app listens there for the one
 *   login it started, and puts a random value in the path to tell it from
 *   any other request.
 */
export function nativeAppRedirect(
  redirect: string | undefined,
  jwt: string,
): string | undefined {
  if (redirect === swiftAppRedirect) {
    return `${swiftAppRedirect}?jwt=${encodeURIComponent(jwt)}`;
  }
  const url = loopbackRedirect(redirect);
  if (!url) {
    return undefined;
  }
  url.searchParams.set('jwt', jwt);
  return url.href;
}

function loopbackRedirect(redirect: string | undefined): URL | undefined {
  if (!redirect) {
    return undefined;
  }
  let url: URL;
  try {
    url = new URL(redirect);
  } catch {
    return undefined;
  }
  const loopback = url.hostname === '127.0.0.1' || url.hostname === '[::1]';
  if (
    url.protocol !== 'http:' ||
    !loopback ||
    url.port === '' ||
    url.username !== '' ||
    url.password !== ''
  ) {
    return undefined;
  }
  url.hash = '';
  return url;
}

/**
 * Whether a request reached the API on the user's own machine, by the
 * hostname Fastify reports. The API serves plain `http` there, so its GitHub
 * callback and its cookie don't ask for `https`.
 */
export function isLocalHostname(hostname: string): boolean {
  return (
    hostname.includes('localhost') ||
    hostname === '127.0.0.1' ||
    hostname === '[::1]'
  );
}
