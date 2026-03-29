/* SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0 */
/*
 * copt_nlp_shim.cpp — Bridge from Rust NlpProblem callbacks to COPT C++ NLP interface.
 *
 * COPT 8.x's NLP callback interface requires a C++ vtable object (INlpCallback).
 * This file is compiled into a standalone shared library (libsurge_copt_nlp.so)
 * via scripts/build-copt-nlp-shim.sh. It exposes a single extern "C" function:
 * copt_nlp_solve(), which surge-opf loads at runtime via libloading.
 *
 * NOTE: nlpcallback.h is missing from the COPT 8.x distribution, so we reconstruct
 * INlpCallback from the virtual method signatures declared in nlpcallbackbase.h.
 * The vtable order (destructor, EvalObj, EvalGrad, EvalCon, EvalJac, EvalHess, getProxy)
 * matches what libcopt_cpp.so expects, as verified by the HS071 test case.
 */

// ── Symbol visibility ────────────────────────────────────────────────────────
#if defined(_WIN32)
  #define SHIM_EXPORT __declspec(dllexport)
#else
  #define SHIM_EXPORT __attribute__((visibility("default")))
#endif

#include "coptcpp_pch.h"
#include <memory>
#include <vector>

// Suppress COPT license warnings written to stdout during Envr() construction.
#if !defined(_WIN32)
#include <unistd.h>
#include <fcntl.h>
struct StdoutSuppressor {
    int saved_fd = -1;
    StdoutSuppressor() {
        saved_fd = dup(STDOUT_FILENO);
        if (saved_fd >= 0) {
            int devnull = open("/dev/null", O_WRONLY);
            if (devnull >= 0) { dup2(devnull, STDOUT_FILENO); close(devnull); }
            else { close(saved_fd); saved_fd = -1; }
        }
    }
    ~StdoutSuppressor() {
        if (saved_fd >= 0) { dup2(saved_fd, STDOUT_FILENO); close(saved_fd); }
    }
};
#else
struct StdoutSuppressor {}; // no-op on Windows
#endif

// ── Reconstructed INlpCallback ────────────────────────────────────────────────
//
// nlpcallback.h is not distributed with COPT 8.x.  We reconstruct the pure-virtual
// interface from the overrides declared in nlpcallbackbase.h.  The vtable layout
// matches libcopt_cpp.so (verified against HS071 known solution = 17.014017145).

class INlpCallback {
public:
    virtual ~INlpCallback() = default;
    virtual int EvalObj (Copt::INdArray<double,1>* xdata, Copt::INdArray<double,1>* outdata) = 0;
    virtual int EvalGrad(Copt::INdArray<double,1>* xdata, Copt::INdArray<double,1>* outdata) = 0;
    virtual int EvalCon (Copt::INdArray<double,1>* xdata, Copt::INdArray<double,1>* outdata) = 0;
    virtual int EvalJac (Copt::INdArray<double,1>* xdata, Copt::INdArray<double,1>* outdata) = 0;
    virtual int EvalHess(Copt::INdArray<double,1>* xdata, double sigma,
                         Copt::INdArray<double,1>* lambdata,
                         Copt::INdArray<double,1>* outdata) = 0;
    virtual Copt::INlpCallbackProxy* getProxy() const = 0;
};

// ── NLP base (replaces NlpCallbackBase) ──────────────────────────────────────

class OurNlpBase : public INlpCallback {
    std::shared_ptr<Copt::INlpCallbackProxy> m_proxy;
public:
    OurNlpBase() : m_proxy(CreateNlpCallbackProxy()) {}
    Copt::INlpCallbackProxy* getProxy() const override { return &(*m_proxy); }
};

// ── Helpers ───────────────────────────────────────────────────────────────────

// Item/SetItem with size_t cast to resolve overload ambiguity between
// T Item(size_t) and INdArray* Item(const IView*).
static inline double ndget(Copt::INdArray<double,1>* a, int i) {
    return a->Item(static_cast<size_t>(i));
}
static inline void ndset(Copt::INdArray<double,1>* a, int i, double v) {
    a->SetItem(static_cast<size_t>(i), v);
}

// ── C-compatible function-pointer struct (matches Rust repr(C)) ──────────────

extern "C" {
    struct CoptNlpFns {
        void* userdata;
        // Evaluate objective f(x) → f_out[0].  Returns 0 on success.
        int (*eval_obj) (int n, const double* x, double* f_out,  void* ud);
        // Evaluate ∇f(x) → grad_out[0..n].
        int (*eval_grad)(int n, const double* x, double* grad_out, void* ud);
        // Evaluate constraint vector g(x) → g_out[0..m].
        int (*eval_con) (int n, int m, const double* x, double* g_out, void* ud);
        // Evaluate sparse Jacobian values → vals_out[0..nnz_jac].
        int (*eval_jac) (int n, int nnz, const double* x, double* vals_out, void* ud);
        // Evaluate lower-triangle Hessian of Lagrangian → vals_out[0..nnz_hess].
        int (*eval_hess)(int n, int m, int nnz, const double* x,
                         double sigma, const double* lambda,
                         double* vals_out, void* ud);
        // Dimensions (set by copt_nlp_solve; not written by Rust).
        int n, m, nnz_jac, nnz_hess;
    };
} // extern "C"

// ── Rust NLP callback adapter ─────────────────────────────────────────────────

class RustNlpCb final : public OurNlpBase {
    const CoptNlpFns& fns_;
public:
    explicit RustNlpCb(const CoptNlpFns& fns) : fns_(fns) {}

    int EvalObj(Copt::INdArray<double,1>* xdata, Copt::INdArray<double,1>* outdata) override {
        std::vector<double> x(fns_.n);
        for (int i = 0; i < fns_.n; ++i) x[i] = ndget(xdata, i);
        double f = 0.0;
        int rc = fns_.eval_obj(fns_.n, x.data(), &f, fns_.userdata);
        if (rc != 0) return rc;
        ndset(outdata, 0, f);
        return 0;
    }

    int EvalGrad(Copt::INdArray<double,1>* xdata, Copt::INdArray<double,1>* outdata) override {
        std::vector<double> x(fns_.n), grad(fns_.n, 0.0);
        for (int i = 0; i < fns_.n; ++i) x[i] = ndget(xdata, i);
        int rc = fns_.eval_grad(fns_.n, x.data(), grad.data(), fns_.userdata);
        if (rc != 0) return rc;
        for (int i = 0; i < fns_.n; ++i) ndset(outdata, i, grad[i]);
        return 0;
    }

    int EvalCon(Copt::INdArray<double,1>* xdata, Copt::INdArray<double,1>* outdata) override {
        std::vector<double> x(fns_.n), g(fns_.m, 0.0);
        for (int i = 0; i < fns_.n; ++i) x[i] = ndget(xdata, i);
        int rc = fns_.eval_con(fns_.n, fns_.m, x.data(), g.data(), fns_.userdata);
        if (rc != 0) return rc;
        for (int i = 0; i < fns_.m; ++i) ndset(outdata, i, g[i]);
        return 0;
    }

    int EvalJac(Copt::INdArray<double,1>* xdata, Copt::INdArray<double,1>* outdata) override {
        std::vector<double> x(fns_.n), vals(fns_.nnz_jac, 0.0);
        for (int i = 0; i < fns_.n; ++i) x[i] = ndget(xdata, i);
        int rc = fns_.eval_jac(fns_.n, fns_.nnz_jac, x.data(), vals.data(), fns_.userdata);
        if (rc != 0) return rc;
        for (int i = 0; i < fns_.nnz_jac; ++i) ndset(outdata, i, vals[i]);
        return 0;
    }

    int EvalHess(Copt::INdArray<double,1>* xdata, double sigma,
                 Copt::INdArray<double,1>* lambdata,
                 Copt::INdArray<double,1>* outdata) override {
        std::vector<double> x(fns_.n), lam(fns_.m, 0.0), vals(fns_.nnz_hess, 0.0);
        for (int i = 0; i < fns_.n; ++i) x[i] = ndget(xdata, i);
        for (int i = 0; i < fns_.m; ++i) lam[i] = ndget(lambdata, i);
        int rc = fns_.eval_hess(fns_.n, fns_.m, fns_.nnz_hess,
                                x.data(), sigma, lam.data(), vals.data(), fns_.userdata);
        if (rc != 0) return rc;
        for (int i = 0; i < fns_.nnz_hess; ++i) ndset(outdata, i, vals[i]);
        return 0;
    }
};

// ── copt_nlp_solve ─────────────────────────────────────────────────────────────
//
// Called from Rust to solve an NLP problem via COPT callback interface.
//
// Parameters:
//   n_obj_grad = COPT_DENSETYPE_ROWMAJOR (-1) → dense gradient; obj_grad_idx = NULL
//   nnz_jac    > 0 → sparse Jacobian; = COPT_DENSETYPE_ROWMAJOR → dense Jacobian
//   nnz_hess   > 0 → exact Hessian provided; = 0 → L-BFGS (no Hessian callback)
//   print_level: 0 = silent, 1+ = verbose
//   time_limit_secs: ≤ 0 → no limit
//
// Returns 0 on success, nonzero on failure.
// On success: objval_out, x_out[n_col], lambda_out[n_row], status_out set.

extern "C" SHIM_EXPORT int copt_nlp_solve(
    int    n_col,
    int    n_row,
    int    n_obj_grad,          // COPT_DENSETYPE_ROWMAJOR or count of sparse entries
    const int*    obj_grad_idx, // NULL if dense gradient
    int    nnz_jac,
    const int*    jac_row,
    const int*    jac_col,
    int    nnz_hess,
    const int*    hess_row,
    const int*    hess_col,
    const double* col_lo,
    const double* col_hi,
    const double* row_lo,
    const double* row_hi,
    const double* init_x,
    int    print_level,
    double time_limit_secs,
    double tol,
    int    max_iter,
    const CoptNlpFns* fns_in,
    double* objval_out,
    double* x_out,
    double* lambda_out,
    int*    status_out)
{
    *status_out = COPT_LPSTATUS_UNSTARTED;

    // Build a mutable copy with dimensions filled in (used by RustNlpCb).
    CoptNlpFns fns = *fns_in;
    fns.n       = n_col;
    fns.m       = n_row;
    fns.nnz_jac  = (nnz_jac  > 0) ? nnz_jac  : n_col * n_row;  // dense fallback
    fns.nnz_hess = (nnz_hess > 0) ? nnz_hess : 0;

    try {
        Envr env = [&]() {
            StdoutSuppressor guard;
            return Envr();
        }();
        Model model = env.CreateModel("copt_nlp");

        // ── Parameters ────────────────────────────────────────────────────────
        model.SetIntParam(COPT_INTPARAM_LOGTOCONSOLE, print_level > 0 ? 1 : 0);
        model.SetIntParam(COPT_INTPARAM_LOGGING,      print_level > 0 ? 1 : 0);
        // COPT NLP spawns internal threads; on large problems (2000+ buses) its
        // parallel NLP kernel hits SIGILL on this machine.  Force single-threaded
        // until Cardinal Operations confirms multi-thread NLP support on Zen 5.
        model.SetIntParam(COPT_INTPARAM_THREADS, 1);

        if (tol > 0.0) {
            model.SetDblParam(COPT_DBLPARAM_FEASTOL, tol);
            model.SetDblParam(COPT_DBLPARAM_NLPTOL,  tol);
        }
        if (time_limit_secs > 0.0)
            model.SetDblParam(COPT_DBLPARAM_TIMELIMIT, time_limit_secs);
        if (max_iter > 0)
            model.SetIntParam(COPT_INTPARAM_NLPITERLIMIT, max_iter);

        // ── evalType bitmask ──────────────────────────────────────────────────
        // 1=ObjVal, 2=ConstrVal, 4=Gradient, 8=Jacobian, 16=Hessian; -1=all.
        int eval_type = COPT_EVALTYPE_OBJVAL | COPT_EVALTYPE_CONSTRVAL
                      | COPT_EVALTYPE_GRADIENT | COPT_EVALTYPE_JACOBIAN;
        if (nnz_hess > 0) eval_type |= COPT_EVALTYPE_HESSIAN;

        // ── Load NLP data and solve ───────────────────────────────────────────
        RustNlpCb cb(fns);

        model.LoadNlData(
            n_col, n_row,
            COPT_MINIMIZE,
            n_obj_grad, obj_grad_idx,  // gradient sparsity/dense flag
            nnz_jac,    jac_row, jac_col,
            nnz_hess,   hess_row, hess_col,
            col_lo, col_hi,
            row_lo, row_hi,
            init_x,
            eval_type,
            &cb);

        model.Solve();

        // ── Extract results ───────────────────────────────────────────────────
        int stat = model.GetIntAttr(COPT_INTATTR_LPSTATUS);
        *status_out = stat;

        if (model.GetIntAttr(COPT_INTATTR_HASLPSOL)) {
            *objval_out = model.GetDblAttr(COPT_DBLATTR_LPOBJVAL);
            // GetLpSolution(colVal, rowSlack, rowDual, redCost)
            model.GetLpSolution(x_out, nullptr, lambda_out, nullptr);
        }

        return 0;

    } catch (CoptException& e) {
        return e.GetCode() != 0 ? e.GetCode() : -1;
    } catch (...) {
        return -2;
    }
}
