# Runs fake_nntp_server's handler behind TLS (self-signed) for testing the TLS path.
import importlib.util, ssl, sys
spec = importlib.util.spec_from_file_location("f", sys.argv[2]); f = importlib.util.module_from_spec(spec); spec.loader.exec_module(f)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); ctx.load_cert_chain(sys.argv[3], sys.argv[4])
srv = f.S(("127.0.0.1", int(sys.argv[1])), f.H)
srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
srv.serve_forever()
