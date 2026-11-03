// skip a random number of calls so we don't start block-aligned
const skip = (Date.now()%50)|0; for(let i=0;i<skip;i++)Math.random();
const out=[]; for(let i=0;i<200;i++) out.push((Math.random()*62)|0);
console.log(JSON.stringify({skip,out}));
