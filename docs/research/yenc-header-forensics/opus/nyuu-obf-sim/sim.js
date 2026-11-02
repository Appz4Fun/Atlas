// Own re-implementation of Nyuu-Obfuscation v0.4.2 Math.random draw order (simulation, not the real binary).
// usage: node sim.js N Q retryArticle(-1 none) warmup
const N=+process.argv[2]||60, Q=+process.argv[3]||1, RETRY=+(process.argv[4]??-1), WARM=+(process.argv[5]??((Date.now()%97)|0));
for(let i=0;i<WARM;i++) Math.random();
const chars='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
const randStr=l=>{let r='';while(l--)r+=chars[(Math.random()*chars.length)|0];return r;};
const files=[{parts:Math.ceil(N/2)},{parts:N-Math.ceil(N/2)}];
const arts=[]; for(let f=0;f<2;f++) for(let p=1;p<=files[f].parts;p++) arts.push({file:f,part:p});
const log=[];
function build(a){
  const f=files[a.file];
  if(f.fnl===undefined){ f.fnl=Math.floor(Math.random()*17)+16; } // first generate() of file only
  a.buildMsgId=randStr(32)+'@'+randStr(8)+'.'+randStr(3);           // createMessageId at build (later overwritten)
  a.subject=randStr(32);                                              // header loop: Subject
  a.from=randStr(14)+' <'+randStr(14)+'@'+randStr(5)+'.'+randStr(3)+'>'; // From
  a.dateOffMs=Math.floor(Math.random()*6*3600*1000);                  // Date (obfuscated)
  log.push('B'+arts.indexOf(a));
}
function send(a){
  a.wireMsgId=randStr(32)+'@'+randStr(8)+'.'+randStr(3);             // nntp.js:828 randomizeMessageID
  if(arts.indexOf(a)===RETRY){ a.firstAttemptMsgId=a.wireMsgId; a.wireMsgId=randStr(32)+'@'+randStr(8)+'.'+randStr(3); }
  log.push('S'+arts.indexOf(a));
}
for(let i=0;i<Q&&i<arts.length;i++) build(arts[i]);
for(let i=0;i<arts.length;i++){ if(i+Q<arts.length) build(arts[i+Q]); send(arts[i]); }
console.log(JSON.stringify({N,Q,RETRY,WARM,order:log.join(' '),arts}));
