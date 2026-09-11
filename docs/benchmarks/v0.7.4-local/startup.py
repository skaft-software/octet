import errno, fcntl, hashlib, json, os, pathlib, platform, pty, select, struct, subprocess, sys, tempfile, termios, time
binary = str(pathlib.Path(sys.argv[1]).resolve())
output = pathlib.Path(sys.argv[2])
assert not output.exists()
trials=[]
for trial in range(int(os.environ.get("REPETITIONS", "9"))):
    with tempfile.TemporaryDirectory(dir=output.parent) as root:
        root=pathlib.Path(root).resolve(); home=root/'home'; workspace=root/'workspace'; sessions=root/'sessions'
        (home/'.octet/credentials').mkdir(parents=True, mode=0o700); workspace.mkdir(); sessions.mkdir()
        credential=home/'.octet/credentials/custom.json'
        credential.write_text(json.dumps(dict(base_url='http://127.0.0.1:9/v1/', api_key='',api_name='probe',headers=[],models=[],auto_discover=False)))
        credential.chmod(0o600)
        env=dict(HOME=str(home),PATH='/usr/bin:/bin',PWD=str(workspace),TERM='xterm-256color',COLORTERM='truecolor',LANG='C.UTF-8',OCTET_COLOR_SCHEME='dark')
        master, slave=pty.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',24,80,0,0))
        def preexec():
            os.setsid(); fcntl.ioctl(slave,termios.TIOCSCTTY,0)
        args=[binary,'--offline','--color','never','--mouse','auto','--workspace',str(workspace),'--session-dir',str(sessions),'--model','custom/probe']
        print('spawning',trial,flush=True); start=time.monotonic_ns(); child=subprocess.Popen(args,env=env,cwd=workspace,stdin=slave,stdout=slave,stderr=slave,preexec_fn=preexec)
        print('spawned',child.pid,flush=True); buf=b''; record=dict(trial=trial); edit_at=None; edit_start=None; response_cursor=0
        try:
            deadline=time.monotonic()+10
            while time.monotonic()<deadline:
                if child.poll() is not None: raise RuntimeError('premature exit '+str(child.returncode))
                readable,_,_=select.select([master],[],[],max(0,deadline-time.monotonic()))
                if not readable: break
                data=os.read(master,65536); buf+=data; now=time.monotonic_ns()
                # Answer terminal cursor position queries like an idle terminal.
                while b'\x1b[6n' in buf[response_cursor:]:
                    response_cursor=buf.index(b'\x1b[6n',response_cursor)+4; os.write(master,b'\x1b[1;1R')
                if 'first_completed_frame_ms' not in record and b'\x1b[?2026l' in buf:
                    record['first_completed_frame_ms']=(now-start)/1e6
                if edit_at is None and b'custom/probe' in buf and b'\x1b[?2026l' in buf[buf.index(b'custom/probe'):]:
                    record['model_label_completed_frame_ms']=(now-start)/1e6
                    edit_at=len(buf); edit_start=time.monotonic_ns(); os.write(master,b'startup_latency_marker')
                if edit_at is not None and b'startup_latency_marker' in buf[edit_at:] and b'\x1b[?2026l' in buf[edit_at+buf[edit_at:].index(b'startup_latency_marker'):]:
                    record['edit_to_completed_frame_ms']=(now-edit_start)/1e6
                    record['spawn_to_editable_frame_ms']=(now-start)/1e6
                    break
            else: raise RuntimeError('deadline')
            if 'spawn_to_editable_frame_ms' not in record: raise RuntimeError('readiness timeout')
            os.write(master,b'\x04')
            deadline=time.monotonic()+5
            while child.poll() is None and time.monotonic()<deadline:
                if select.select([master],[],[],0.05)[0]:
                    try: buf+=os.read(master,65536)
                    except OSError as e:
                        if e.errno != errno.EIO: raise
            record['exit']=child.wait(timeout=0.1)
        except Exception as e:
            record['error']=str(e); record['transcript']=repr(buf[-12000:]); print(record,flush=True)
        finally:
            if child.poll() is None: child.kill(); os.close(master); master=None; child.wait(timeout=3)
            if master is not None: os.close(master)
            os.close(slave)
        trials.append(record)
        print(record,flush=True)
output.write_text(json.dumps(dict(binary=binary,sha256=hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),platform=platform.platform(),profile='dev',scope='offline manual model, default core tools, no extensions, 80x24 PTY; no prompt submitted, no provider readiness inference',trials=trials),indent=2))
