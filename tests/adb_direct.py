"""Small direct smart-socket driver for device input/screenshot verification, no adb CLI."""
import socket,struct,shlex

class Adb:
    def __init__(self, serial, host='127.0.0.1', port=5037):
        assert serial and not any(c in serial for c in '\r\n\0')
        self.serial,self.address=serial,(host,port)

    @staticmethod
    def read(s,n):
        b=bytearray()
        while len(b)<n:
            c=s.recv(n-len(b))
            if not c:raise EOFError('ADB stream ended early')
            b.extend(c)
        return bytes(b)

    def service(self, command):
        s=socket.create_connection(self.address,timeout=45)
        try:
            for request in ['host:transport:'+self.serial,command]:
                b=request.encode();s.sendall(('%04x'%len(b)).encode()+b)
                status=self.read(s,4)
                if status!=b'OKAY':raise RuntimeError(self.read(s,int(self.read(s,4),16)).decode(errors='replace'))
            return s
        except BaseException:
            s.close();raise

    def shell(self,*arguments,check=True):
        with self.service('shell,v2,raw:'+' '.join(shlex.quote(str(a)) for a in arguments)) as s:
            s.sendall(b'\x04\0\0\0\0')
            stdout=bytearray();stderr=bytearray();exit_code=None
            while exit_code is None:
                header=self.read(s,5);kind=header[0];size=struct.unpack('<I',header[1:])[0]
                assert size<=4*1024*1024
                payload=self.read(s,size)
                if kind==1:stdout.extend(payload)
                elif kind==2:stderr.extend(payload)
                elif kind==3:exit_code=int.from_bytes(payload,'little')
                else:raise RuntimeError('unexpected shell packet '+str(kind))
                assert len(stdout)+len(stderr)<=16*1024*1024
        result={'stdout':stdout.decode(errors='replace'),'stderr':stderr.decode(errors='replace'),'exit_code':exit_code}
        if check and exit_code:raise RuntimeError(result)
        return result

    def execute(self,*arguments):
        with self.service('exec:'+' '.join(shlex.quote(str(a)) for a in arguments)) as s:
            data=bytearray()
            while True:
                b=s.recv(65536)
                if not b:return bytes(data)
                data.extend(b);assert len(data)<=32*1024*1024
