import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import { parseDaemonStatus } from '../src/frost/cli';

test('daemon status accepts every versioned state and additive fields', () => {
  const status = parseDaemonStatus(JSON.stringify({
    schema: 'frost-daemon-status-v1',
    state: 'protocol_mismatch',
    protocol: 1,
    expected_protocol: 2,
    future: true,
  }));
  assert.equal(status.state, 'protocol_mismatch');
  assert.equal(status.protocol, 1);
});

test('daemon status rejects prose and unknown schemas', () => {
  assert.throws(() => parseDaemonStatus('frostd: stopped'), /JSON/);
  assert.throws(
    () => parseDaemonStatus(JSON.stringify({
      schema: 'frost-daemon-status-v2',
      state: 'running',
      protocol: 2,
      expected_protocol: 2,
    })),
    /unsupported payload/,
  );
});
