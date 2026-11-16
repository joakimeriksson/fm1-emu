import {createServer} from "node:http";
import {readFile,realpath,stat} from "node:fs/promises";
import {resolve,extname,sep} from "node:path";
const root=await realpath("web/dist");
const mime={".ico":"image/vnd.microsoft.icon",".webmanifest":"application/manifest+json",".html":"text/html; charset=utf-8",".js":"text/javascript; charset=utf-8",".css":"text/css; charset=utf-8",".wasm":"application/wasm",".json":"application/json",".png":"image/png",".pdf":"application/pdf"};
createServer(async(req,res)=>{
  try {
    const pathname=decodeURIComponent(new URL(req.url,"http://localhost").pathname);
    let path=resolve(root,"."+pathname);
    if(!path.startsWith(root+sep)&&path!==root) {res.writeHead(403).end();return;}
    if((await stat(path)).isDirectory()) path=resolve(path,"index.html");
    path=await realpath(path);
    if(!path.startsWith(root+sep)) {res.writeHead(403).end();return;}
    const data=await readFile(path);
    res.writeHead(200,{"Content-Type":mime[extname(path)]||"application/octet-stream","Cache-Control":"no-store","X-Content-Type-Options":"nosniff"});
    res.end(data);
  } catch {res.writeHead(404,{"Content-Type":"text/plain"}).end("Not found");}
}).listen(Number(process.env.PORT||8642),"127.0.0.1",()=>console.log("FM-1 Studio: http://127.0.0.1:"+(process.env.PORT||8642)+"/studio/"));
