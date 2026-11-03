import math
p=0.0158
pres={'Nyuu/ngPost 716800':716800,'postie 750000':750000,'NewsUP 768000':768000,'PowerPost 640000':640000,'384000':384000,'512000':512000,'GoPostStuff/other 500000':500000,'1MB 1048576':1048576}
print('preset  L  body_lines(+3 y-lines)  sd_lines  body_bytes~')
for k,P in pres.items():
  for L in (128,256):
    m=P*(1+p); sd=math.sqrt(P*p*(1-p))
    print(f'{k:28s} {L} {math.ceil(m/L)+3:6d} {sd/L:5.2f} {int(m+2*(m/L)+150):8d}')
# bits from last-part: resolution
for P in (716800,768000):
  for tol in (1,2,3):
    print(P,'tol',tol,'buckets',P*(1+p)/128/(2*tol+1), 'bits',round(math.log2(P*(1+p)/128/(2*tol+1)),2))
