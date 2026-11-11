import {expect, test} from 'vitest';
import {isLocalHostname, nativeAppRedirect} from './native-login.ts';

const jwt = 'a.b+c/d=';

test('the Swift app gets the token at its scheme', () => {
  expect(nativeAppRedirect('zbugs-swift://auth', jwt)).toBe(
    'zbugs-swift://auth?jwt=a.b%2Bc%2Fd%3D',
  );
});

test('a loopback redirect gets the token in its query', () => {
  expect(nativeAppRedirect('http://127.0.0.1:53817/auth/x-Y_1', jwt)).toBe(
    'http://127.0.0.1:53817/auth/x-Y_1?jwt=a.b%2Bc%2Fd%3D',
  );
  expect(nativeAppRedirect('http://[::1]:53817/auth?state=s', jwt)).toBe(
    'http://[::1]:53817/auth?state=s&jwt=a.b%2Bc%2Fd%3D',
  );
});

test('anything else is the web app’s redirect', () => {
  for (const redirect of [
    undefined,
    '',
    '/',
    '/issue/123',
    'zbugs-swift://other',
    // Not loopback, or not plain http to a port on it.
    'http://localhost:53817/auth',
    'http://127.0.0.2:53817/auth',
    'http://127.0.0.1.example.com:53817/auth',
    'http://example.com/?127.0.0.1:53817',
    'https://127.0.0.1:53817/auth',
    'http://127.0.0.1/auth',
    'http://127.0.0.1:80/auth',
    'http://evil@127.0.0.1:53817/auth',
    'javascript:alert(1)',
    'not a url',
  ]) {
    expect(nativeAppRedirect(redirect, jwt), String(redirect)).toBe(undefined);
  }
});

test('the API is local at localhost and the loopback addresses', () => {
  for (const hostname of ['localhost', '127.0.0.1', '[::1]']) {
    expect(isLocalHostname(hostname)).toBe(true);
  }
  for (const hostname of [
    'bugs.rocicorp.dev',
    '127.0.0.2',
    '::1',
    '10.0.0.1',
  ]) {
    expect(isLocalHostname(hostname)).toBe(false);
  }
});
