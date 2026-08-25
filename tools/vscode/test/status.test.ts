import * as assert from 'node:assert/strict';
import { after, before, test } from 'node:test';

import { installVscodeStub, type Recorded } from './vscode-stub';

let stub: { recorded: Recorded; dispose: () => void };
let recorded: Recorded;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let createStatusReporter: any;

before(() => {
  stub = installVscodeStub();
  recorded = stub.recorded;
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  createStatusReporter = require('../src/status').createStatusReporter;
});

after(() => stub.dispose());

test('daemon refresh preserves the last build result in the status bar', () => {
  const status = createStatusReporter();
  status.succeeded('FrostBuild: 1 built');
  status.daemon({
    schema: 'frost-daemon-status-v1',
    state: 'running',
    protocol: 2,
    expected_protocol: 2,
  });
  assert.match(recorded.status.text, /check.*vm-active/);
  assert.match(recorded.status.tooltip, /1 built\nDaemon: running \(protocol 2\)/);

  status.daemon({
    schema: 'frost-daemon-status-v1',
    state: 'protocol_mismatch',
    protocol: 1,
    expected_protocol: 2,
  });
  assert.match(recorded.status.text, /check.*warning/);
  assert.match(recorded.status.tooltip, /expects 2/);
});
