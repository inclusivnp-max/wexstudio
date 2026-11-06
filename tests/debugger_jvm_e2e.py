#!/usr/bin/env python3
"""Real pinned Java Debug Server/JDI regression; distinct from Android/phone evidence."""
from pathlib import Path
import hashlib
import json
import os
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
JAVA = Path(os.environ.get('WEX_DEBUG_JAVA_HOME', '/usr/lib/jvm/java-21-openjdk')) / 'bin'
SOURCE = '''package probe;
public class DebugProbe {
    public static void main(String[] args) {
        int result = descend(70);
        System.out.println("JVM_DEBUG_RESULT=" + result);
    }
    static int descend(int depth) {
        if (depth == 0) return probe(7);
        int result = descend(depth - 1);
        return result;
    }
    static int probe(int seed) {
        int doubled = seed * 2;
        int[] samples = new int[250];
        for (int i = 0; i < samples.length; i++) samples[i] = i * 3;
        int answer = nested(doubled);
        return answer;
    }
    static int nested(int value) {
        int adjusted = value + 3;
        return adjusted;
    }
}
'''


class Protocol:
    def __init__(self, process):
        self.process = process
        self.seq = 0
        self.incoming = queue.Queue()
        self.events = []
        self.trace = []
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        try:
            while True:
                size = None
                while True:
                    line = self.process.stdout.readline()
                    if not line:
                        raise EOFError('DAP adapter stdout closed')
                    if line == b'\r\n':
                        break
                    if line.lower().startswith(b'content-length:'):
                        size = int(line.split(b':')[1])
                assert size is not None and 0 < size < 8 * 1024 * 1024
                payload = self.process.stdout.read(size)
                assert len(payload) == size
                self.incoming.put(json.loads(payload))
        except Exception as error:
            self.incoming.put(error)

    def receive(self, deadline):
        message = self.incoming.get(timeout=max(0.01, deadline - time.monotonic()))
        if isinstance(message, Exception):
            raise message
        self.trace.append({'direction': 'recv', 'message': message})
        return message

    def request(self, command, arguments=None):
        self.seq += 1
        seq = self.seq
        message = dict(seq=seq, type='request', command=command, arguments=arguments or {})
        self.trace.append({'direction': 'send', 'message': message})
        payload = json.dumps(message).encode()
        self.process.stdin.write(b'Content-Length: ' + str(len(payload)).encode() + b'\r\n\r\n' + payload)
        self.process.stdin.flush()
        deadline = time.monotonic() + 20
        while True:
            reply = self.receive(deadline)
            if reply['type'] == 'event':
                self.events.append(reply)
                continue
            assert reply['type'] == 'response' and reply['request_seq'] == seq, reply
            assert reply['success'], reply
            return reply.get('body', {})

    def event(self, name):
        deadline = time.monotonic() + 20
        while True:
            for index, event in enumerate(self.events):
                if event['event'] == name:
                    return self.events.pop(index).get('body', {})
            reply = self.receive(deadline)
            assert reply['type'] == 'event', reply
            self.events.append(reply)


def main():
    output = Path(sys.argv[1] if len(sys.argv) > 1 else '.verification/jvm-debug.json').resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(['python3', str(ROOT / 'scripts/prepare-debugger.py'), '--offline'], check=True)
    with tempfile.TemporaryDirectory(prefix='wex-jdi-') as folder:
        folder = Path(folder)
        source = folder / 'DebugProbe.java'
        source.write_text(SOURCE)
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        build = folder / 'build'
        classes = build / 'classes/app'
        classes.mkdir(parents=True)
        subprocess.run([str(JAVA / 'javac'), '-g', '-d', str(classes), str(source)], check=True)
        sources = folder / 'sources.json'
        sources.write_text(json.dumps([str(source)]))
        logs = queue.Queue()
        stdout = []
        vm = subprocess.Popen([str(JAVA / 'java'), '-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:0', '-cp', str(classes), 'probe.DebugProbe'], stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        adapter = None
        def collect():
            for line in vm.stdout:
                line = line.decode().strip()
                stdout.append(line)
                logs.put(line)
        collector = threading.Thread(target=collect, daemon=True)
        collector.start()
        try:
            match = re.search(r'address: (\d+)', logs.get(timeout=10))
            assert match, stdout
            cp = os.pathsep.join([str(ROOT / '.wex/debugger/classes'), str(ROOT / '.wex/debugger/lib/*')])
            adapter = subprocess.Popen([str(JAVA / 'java'), '--add-modules', 'jdk.jdi', '-cp', cp, 'WexDebugServer', str(sources), str(build)], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
            dap = Protocol(adapter)
            capabilities = dap.request('initialize', dict(clientID='wex-jvm-regression', adapterID='java', linesStartAt1=True, columnsStartAt1=True, pathFormat='path'))
            assert capabilities.get('supportsDelayedStackTraceLoading') is False, capabilities
            dap.request('attach', dict(hostName='127.0.0.1', port=int(match[1]), timeout=10000, sourcePaths=[str(folder)]))
            line = SOURCE.splitlines().index('        int answer = nested(doubled);') + 1
            arguments = dict(source=dict(path=str(source), name=source.name), breakpoints=[dict(line=line)], sourceModified=False)
            dap.request('setBreakpoints', arguments)
            dap.request('configurationDone')
            snapshots = {}
            def inspect(label):
                event = dap.event('stopped')
                thread = event['threadId']
                frames = dap.request('stackTrace', dict(threadId=thread))['stackFrames']
                scopes = dap.request('scopes', dict(frameId=frames[0]['id']))['scopes']
                variables = []
                for scope in scopes:
                    variables.extend(dap.request('variables', dict(variablesReference=scope['variablesReference']))['variables'])
                snapshots[label] = dict(event=event, frames=frames, variables=variables)
                return thread, {v['name']: v['value'] for v in variables}
            thread, values = inspect('breakpoint')
            assert values['seed'] == '7' and values['doubled'] == '14', values
            complete = snapshots['breakpoint']['frames']
            assert len(complete) == 73, complete
            assert 'main' in complete[-1]['name'], complete[-1]
            deep_scope = dap.request('scopes', dict(frameId=complete[61]['id']))['scopes']
            deep_variables = [v for scope in deep_scope for v in dap.request('variables', dict(variablesReference=scope['variablesReference']))['variables']]
            assert any(v['name'] == 'depth' and v['value'] == '60' for v in deep_variables), deep_variables
            snapshots['deepCaller'] = dict(frame=complete[61], variables=deep_variables)

            array = next(v for v in snapshots['breakpoint']['variables'] if v['name'] == 'samples')
            assert array['type'] == 'int[]', array
            pages = []
            for start,count in [(0,100),(100,100),(200,50)]:
                page = dap.request('variables', dict(variablesReference=array['variablesReference'], filter='indexed', start=start, count=count))['variables']
                assert len(page) == min(count,250-start), page
                pages.extend(page)
            assert len(pages) == 250 and len({v['name'] for v in pages}) == 250
            assert pages[-1]['value'] == '747', pages[-1]
            snapshots['arrayPages'] = dict(array=array, loaded=len(pages), last=pages[-1], page_sizes=[100,100,50])
            assert len(dap.request('threads')['threads']) >= 1
            dap.request('stepIn', dict(threadId=thread))
            thread, values = inspect('stepIn')
            assert values['value'] == '14', values
            dap.request('next', dict(threadId=thread))
            thread, values = inspect('next')
            assert values['adjusted'] == '17', values
            dap.request('stepOut', dict(threadId=thread))
            thread, values = inspect('stepOut')
            assert any(v == '17' for v in values.values()), values
            if 'answer' not in values:
                dap.request('next', dict(threadId=thread))
                thread, values = inspect('assignment')
            assert values['answer'] == '17', values
            assert len(snapshots['breakpoint']['frames']) >= 2
            assert Path(snapshots['breakpoint']['frames'][0]['source']['path']).resolve() == source
            arguments['breakpoints'] = []
            assert dap.request('setBreakpoints', arguments)['breakpoints'] == []
            dap.request('continue', dict(threadId=thread))
            dap.event('terminated')
            assert vm.wait(timeout=10) == 0
            collector.join(timeout=5)
            assert not collector.is_alive()
            assert 'JVM_DEBUG_RESULT=17' in stdout, stdout
            assert hashlib.sha256(source.read_bytes()).hexdigest() == digest
            output.write_text(json.dumps(dict(result='JVM_DEBUG_OK', adapter='Microsoft Java Debug Server 0.53.1', runtime='JDK21 local JVM (not Android)', snapshots=snapshots, stdout=stdout, trace=dap.trace, source_unchanged=True), indent=2) + '\n')
            print('JVM_DEBUG_OK: real Java Debug Server/JDI; breakpoint 7/14; stepIn/next/stepOut 17; complete 73-frame stack/deep caller depth=60; locals; array paging 100/100/50 last=747; removal/continue/termination; source unchanged')
        finally:
            for process in [adapter, vm]:
                if process is not None:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=10)
                    for stream in [process.stdin, process.stdout, process.stderr]:
                        if stream is not None:
                            stream.close()


if __name__ == '__main__':
    main()
