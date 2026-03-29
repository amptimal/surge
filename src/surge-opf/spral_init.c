/* SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0 */
/**
 * SPRAL OpenMP initialization shim.
 *
 * SPRAL's SSIDS solver requires OMP_CANCELLATION=TRUE to use OpenMP task
 * cancellation in its supernodal multifrontal factorization. This needs to
 * be set BEFORE the OpenMP runtime (libgomp) reads environment variables
 * during its first initialization, which happens before any parallel region.
 *
 * Using __attribute__((constructor)) ensures this runs before main() and
 * before any shared library's first parallel region, guaranteeing libgomp
 * sees OMP_CANCELLATION=TRUE when it initializes.
 *
 * We use overwrite=0 so the user can override by setting OMP_CANCELLATION
 * explicitly in their environment before running the binary.
 */
#include <stdlib.h>

/* Priority 101: runs before default constructors (priority 65535),
 * ensuring OMP_CANCELLATION is set before libgomp reads it during its
 * own constructor. Range 0-100 is reserved by the implementation. */
__attribute__((constructor(101)))
static void surge_spral_init(void) {
    setenv("OMP_CANCELLATION", "TRUE", 0 /* don't override user's value */);
}
