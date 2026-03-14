// Mach port-based IOSurface transfer: server side.
//
// Protocol:
//   1. Server registers a Mach service via bootstrap_check_in
//   2. Client looks up the service and sends a "subscribe" message with a reply port
//   3. Server stores the client's send right
//   4. On each on_accelerated_paint, server creates an IOSurface Mach port
//      and sends it + metadata to the client via mach_msg

#include <mach/mach.h>
#include <servers/bootstrap.h>
#include <IOSurface/IOSurface.h>
#include <stdint.h>
#include <stdio.h>

// ---- Message types ----

// Client → Server: subscription request (carries client's port)
typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t client_port;
} subscribe_msg_t;

// Server → Client: IOSurface transfer
typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t surface_port;
    // Inline data (after descriptors)
    uint32_t width;
    uint32_t height;
    uint32_t format;
} iosurface_msg_t;

// ---- Global state ----

static mach_port_t g_server_port = MACH_PORT_NULL;   // receive right (listens for subscriptions)
static mach_port_t g_client_port = MACH_PORT_NULL;   // send right to client

// ---- Server API ----

/// Register a Mach service with the bootstrap server.
/// Returns 0 on success, negative on error.
int mach_iosurface_server_init(const char* service_name) {
    kern_return_t kr;

    // Allocate a receive right
    kr = mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &g_server_port);
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "[mach_iosurface] mach_port_allocate failed: %s\n", mach_error_string(kr));
        return -1;
    }

    // Insert a send right so bootstrap can hold a copy
    kr = mach_port_insert_right(mach_task_self(), g_server_port, g_server_port,
                                MACH_MSG_TYPE_MAKE_SEND);
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "[mach_iosurface] mach_port_insert_right failed: %s\n", mach_error_string(kr));
        return -2;
    }

    // Use bootstrap_register to register OUR port with the bootstrap server.
    // (bootstrap_check_in returns a NEW port via output param, which doesn't route correctly
    //  for dynamically registered services without a launchd plist.)
    #pragma clang diagnostic push
    #pragma clang diagnostic ignored "-Wdeprecated-declarations"
    kr = bootstrap_register(bootstrap_port, service_name, g_server_port);
    #pragma clang diagnostic pop
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "[mach_iosurface] bootstrap_register('%s') failed: %s\n",
                service_name, mach_error_string(kr));
        return -3;
    }

    return 0;
}

/// Non-blocking check for client subscription.
/// Returns 1 if client is connected, 0 if not yet.
int mach_iosurface_server_accept(void) {
    if (g_client_port != MACH_PORT_NULL) return 1;  // already connected
    if (g_server_port == MACH_PORT_NULL) return 0;

    // Receive buffer must include space for mach_msg_trailer_t (8 bytes),
    // otherwise the kernel returns MACH_RCV_TOO_LARGE and destroys the message.
    struct {
        subscribe_msg_t msg;
        mach_msg_trailer_t trailer;
    } recv_buf;
    __builtin_memset(&recv_buf, 0, sizeof(recv_buf));

    kern_return_t kr = mach_msg(
        &recv_buf.msg.header,
        MACH_RCV_MSG | MACH_RCV_TIMEOUT,
        0,                              // send size
        sizeof(recv_buf),               // receive buffer size (includes trailer)
        g_server_port,                  // receive port
        0,                              // timeout = 0 (non-blocking)
        MACH_PORT_NULL
    );

    if (kr == MACH_MSG_SUCCESS) {
        g_client_port = recv_buf.msg.client_port.name;
        return 1;
    }

    return 0;
}

/// Send an IOSurface to the connected client via Mach port.
/// io_surface_ref is a raw IOSurfaceRef pointer.
/// Returns 0 on success, negative on error.
int mach_iosurface_server_send(void* io_surface_ref, uint32_t width, uint32_t height, uint32_t format) {
    if (g_client_port == MACH_PORT_NULL) return -1;

    IOSurfaceRef surface = (IOSurfaceRef)io_surface_ref;
    mach_port_t surface_port = IOSurfaceCreateMachPort(surface);
    if (surface_port == MACH_PORT_NULL) return -2;

    iosurface_msg_t msg;
    __builtin_memset(&msg, 0, sizeof(msg));

    msg.header.msgh_bits = MACH_MSGH_BITS_COMPLEX |
                           MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
    msg.header.msgh_size = sizeof(msg);
    msg.header.msgh_remote_port = g_client_port;
    msg.header.msgh_local_port = MACH_PORT_NULL;
    msg.header.msgh_id = 0x494F5346;  // 'IOSF'

    msg.body.msgh_descriptor_count = 1;

    msg.surface_port.name = surface_port;
    msg.surface_port.disposition = MACH_MSG_TYPE_MOVE_SEND;
    msg.surface_port.type = MACH_MSG_PORT_DESCRIPTOR;

    msg.width = width;
    msg.height = height;
    msg.format = format;

    kern_return_t kr = mach_msg(
        &msg.header,
        MACH_SEND_MSG | MACH_SEND_TIMEOUT,
        sizeof(msg),        // send size
        0,                  // receive size
        MACH_PORT_NULL,     // receive port
        10,                 // 10ms timeout (don't block on_accelerated_paint)
        MACH_PORT_NULL
    );

    if (kr != KERN_SUCCESS) {
        // Failed to send — deallocate the surface port we created
        mach_port_deallocate(mach_task_self(), surface_port);
        if (kr == MACH_SEND_INVALID_DEST) {
            // Client disconnected — reset so we can accept a new one
            g_client_port = MACH_PORT_NULL;
        }
        return -3;
    }

    return 0;
}

/// Check if a client is connected.
int mach_iosurface_server_has_client(void) {
    return g_client_port != MACH_PORT_NULL ? 1 : 0;
}
