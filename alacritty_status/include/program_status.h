/* Opaque, single-threaded OSC 7501 record store. No ownership crosses the ABI. */
#ifndef PROGRAM_STATUS_H
#define PROGRAM_STATUS_H
#include <stddef.h>
void *ps_new(void);
void ps_free(void *);
int ps_apply(void *, const unsigned char *, size_t, int);
int ps_expire(void *, int);
const char *ps_label(void *);
#endif
