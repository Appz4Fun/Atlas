import zlib
T=3862205996
lines=["*** iPhone SUPER-SPECIAL-DISCOUNT 80% OFF --- UNBEATABLE SUPER-PRICES !!! ***","","Save LOTS OF CASH on a wide collections of iPhones, 80% off","everything must go to make place of next generation iPhone","","Bell Super Discounts","http://www.grex.org/~henced/iPhones.html"]
for lead in [0,1,2]:
    for trail in [0,1]:
        L=[""]*lead+lines
        for term in [False,True]:
            body="\r\n".join(L)+("\r\n" if trail else "")
            v=body+(".\r\n" if term else "")
            c=zlib.crc32(v.encode())&0xffffffff
            print(f"lead={lead} trail={trail} dot={term} len={len(v.encode())} crc={c} {'MATCH' if c==T else ''}")
