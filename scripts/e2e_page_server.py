#!/usr/bin/env python3
"""Rust 探测器配套：动画测试页 http server"""
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 18097

PAGE = """<!doctype html><html><head><style>
body{margin:0;background:#111}
.c{width:80px;height:80px;border-radius:12px;position:absolute;
background:linear-gradient(90deg,#f36,#36f);animation:mv 2s infinite alternate}
@keyframes mv{to{transform:translateX(300px)}}
</style></head><body><div class="c" style="top:100px"></div>
<script>
var d2=document.createElement('div');d2.className='c';d2.style.top='300px';
d2.style.animation='none';document.body.appendChild(d2);
var t=0;
function loop(){t+=4;if(t>700)t=0;d2.style.top=(300+t)+'px';requestAnimationFrame(loop)}
requestAnimationFrame(loop);
window.__CPK_TICK__=function(){return 1};
</script></body></html>"""

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "text/html; charset=utf-8")
        self.end_headers()
        self.wfile.write(PAGE.encode())

HTTPServer(("127.0.0.1", PORT), H).serve_forever()
