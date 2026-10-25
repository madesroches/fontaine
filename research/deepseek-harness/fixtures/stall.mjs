// Accepts connections on 127.0.0.1:8099 and never answers: a trigger for the web_fetch timeout.
import http from 'node:http'
http.createServer(() => {}).listen(8099, '127.0.0.1')
