import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

export interface Certificate {
  key: string;
  cert: string;
  /** Deletes the scratch directory that held the key. */
  cleanup(): void;
}

const SUBJECT_ALT_NAME = 'DNS:localhost,IP:127.0.0.1,IP:::1';

const CONFIG = `[req]
distinguished_name = dn
x509_extensions = v3
prompt = no

[dn]
CN = localhost

[v3]
subjectAltName = ${SUBJECT_ALT_NAME}
`;

/** Where the certificate files go: `MOCK_IMMICH_SCRATCH_DIR` when set, the system temp otherwise. */
export function scratchRoot(): string {
  const configured = process.env.MOCK_IMMICH_SCRATCH_DIR;
  if (configured === undefined || configured === '') {
    return tmpdir();
  }
  mkdirSync(configured, { recursive: true });
  return configured;
}

/** Whether the `openssl` command-line tool can be run. */
export function hasOpenssl(): boolean {
  try {
    execFileSync('openssl', ['version'], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

function openssl(args: string[]): void {
  execFileSync('openssl', args, { stdio: ['ignore', 'pipe', 'pipe'], timeout: 60_000 });
}

/**
 * A self-signed certificate for localhost, 127.0.0.1 and ::1, made by the `openssl` command-line
 * tool. The key lives in a scratch directory for as long as the server runs.
 */
export function createSelfSignedCertificate(): Certificate {
  if (!hasOpenssl()) {
    throw new Error('--tls needs the openssl command-line tool, which was not found in PATH');
  }
  const dir = mkdtempSync(join(scratchRoot(), 'mock-immich-tls-'));
  const keyFile = join(dir, 'key.pem');
  const certFile = join(dir, 'cert.pem');
  const common = ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '30'];
  const outputs = ['-keyout', keyFile, '-out', certFile];
  try {
    try {
      openssl([
        ...common,
        ...outputs,
        '-subj',
        '/CN=localhost',
        '-addext',
        `subjectAltName=${SUBJECT_ALT_NAME}`,
      ]);
    } catch {
      // An OpenSSL without `-addext` (before 1.1.1) takes the extension from a config file.
      const configFile = join(dir, 'openssl.cnf');
      writeFileSync(configFile, CONFIG);
      openssl([...common, ...outputs, '-config', configFile]);
    }
    return {
      key: readFileSync(keyFile, 'utf8'),
      cert: readFileSync(certFile, 'utf8'),
      cleanup: () => rmSync(dir, { recursive: true, force: true }),
    };
  } catch (error) {
    rmSync(dir, { recursive: true, force: true });
    const stderr = (error as { stderr?: Buffer }).stderr?.toString('utf8').trim();
    throw new Error(`openssl could not make the certificate: ${stderr || String(error)}`);
  }
}
