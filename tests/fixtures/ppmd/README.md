PPMd variant I (zip method 98) streams made with pyppmd's `Ppmd8Encoder`, each behind the 2-byte zip header
(order - 1, memory MiB - 1 << 4, restore << 12):

- `mixed.ppmd`: `mixed.bin` (text lines, 20 000 random bytes, 30 000 zeros, 0..=255 x 40), order 6, 16 MiB, restart.
- `restart.ppmd`, `cutoff.ppmd`: 80 000 bytes of LCG text (see the tests), order 16, 1 MiB, so memory runs out;
  one starts the model over, the other cuts it back.
- `mixed-256m.ppmd`: `mixed.bin` again, order 8, the most memory a header can ask for (256 MiB), of which the decoder
  only keeps what the model writes.
