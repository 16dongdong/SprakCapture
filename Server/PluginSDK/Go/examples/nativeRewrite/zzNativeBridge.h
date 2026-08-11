#ifndef SPRAK_CAPTURE_GO_NATIVE_BRIDGE_H
#define SPRAK_CAPTURE_GO_NATIVE_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

typedef struct {
    const uint8_t *pointer;
    size_t length;
} NativeByteSlice;

typedef struct {
    uint32_t apiVersion;
    NativeByteSlice manifest;
    NativeByteSlice configuration;
} NativeExtensionInitRequest;

typedef struct {
    const uint8_t *pointer;
    size_t length;
    void *releaseContext;
    void (*release)(void *, const uint8_t *, size_t);
} NativeExtensionBuffer;

typedef struct {
    uint32_t apiVersion;
    void *pluginContext;
    int32_t (*invoke)(void *, NativeByteSlice, NativeExtensionBuffer *);
    void (*stop)(void *);
    void (*destroy)(void *);
} NativeExtensionExports;

extern int32_t goCaptureInvoke(void *, NativeByteSlice, NativeExtensionBuffer *);
extern void goCaptureStop(void *);
extern void goCaptureDestroy(void *);

void *nativeContextCreate(uint64_t);
uint64_t nativeContextID(void *);
void nativeContextFree(void *);
void nativeExportsSet(NativeExtensionExports *, void *);
void nativeBufferSet(NativeExtensionBuffer *, void *, size_t);

#endif
