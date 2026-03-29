# References

Governing references for the methods implemented in Surge. Where a Surge
implementation follows a specific formulation, the reference is cited here.
For method classification (reference-equation, approximation, heuristic), see
[Method Fidelity](method-fidelity.md).

## AC Power Flow

- **[Tinney1967]** W. F. Tinney and C. E. Hart, "Power Flow Solution by
  Newton's Method," *IEEE Transactions on Power Apparatus and Systems*,
  vol. PAS-86, no. 11, pp. 1449-1460, Nov. 1967.
  The foundational Newton-Raphson formulation for power flow in polar
  coordinates. Surge's `solve_ac_pf` implements this formulation with sparse
  Jacobian assembly and KLU factorization.

- **[Stott1974]** B. Stott, "Review of Load-Flow Calculation Methods,"
  *Proceedings of the IEEE*, vol. 62, no. 7, pp. 916-929, July 1974.
  Comprehensive review of power flow methods including NR and fast-decoupled.

- **[Stott1974b]** B. Stott and O. Alsac, "Fast Decoupled Load Flow,"
  *IEEE Transactions on Power Apparatus and Systems*, vol. PAS-93, no. 3,
  pp. 859-869, May 1974.
  The original FDPF formulation. Surge's `solve_fdpf` implements both the
  XB and BX variants described in this paper.

## DC Power Flow And Sensitivities

- **[Stott2009]** B. Stott, J. Jardim, and O. Alsac, "DC Power Flow
  Revisited," *IEEE Transactions on Power Systems*, vol. 24, no. 3,
  pp. 1290-1300, Aug. 2009.
  Modern treatment of the DC power flow approximation, its assumptions
  (flat voltage, small angles, lossless branches), and practical accuracy.
  Surge's `solve_dc` implements this formulation.

- **[Wood2014]** A. J. Wood, B. F. Wollenberg, and G. B. Sheble,
  *Power Generation, Operation, and Control*, 3rd ed., Wiley, 2014.
  Standard textbook reference for PTDF, LODF, shift factors, and their
  use in market operations and contingency screening.

## Optimal Power Flow

- **[Carpentier1962]** J. Carpentier, "Contribution a l'etude du dispatching
  economique," *Bulletin de la Societe Francaise des Electriciens*, vol. 3,
  no. 8, pp. 431-447, 1962.
  Original formulation of the OPF problem.

- **[Capitanescu2011]** F. Capitanescu, J. L. Martinez Ramos, P. Panciatici,
  D. Kirschen, A. Marano Marcolini, L. Platbrood, and L. Wehenkel,
  "State-of-the-art, challenges, and future trends in security constrained
  optimal power flow," *Electric Power Systems Research*, vol. 81, no. 8,
  pp. 1731-1741, 2011.
  Survey of SCOPF formulations including Benders decomposition and
  constraint generation approaches.

- **[Wachter2006]** A. Wachter and L. T. Biegler, "On the implementation of
  an interior-point filter line-search algorithm for large-scale nonlinear
  programming," *Mathematical Programming*, vol. 106, no. 1, pp. 25-57, 2006.
  The Ipopt algorithm used as the default NLP backend for AC-OPF.

## Contingency Analysis

- **[Alsac1974]** O. Alsac and B. Stott, "Optimal Load Flow with
  Steady-State Security," *IEEE Transactions on Power Apparatus and Systems*,
  vol. PAS-93, no. 3, pp. 745-751, 1974.
  Early formulation of security-constrained dispatch.

- **[Kessel1986]** P. Kessel and H. Glavitsch, "Estimating the Voltage
  Stability of a Power System," *IEEE Transactions on Power Delivery*,
  vol. PWRD-1, no. 3, pp. 346-354, July 1986.
  The L-index voltage stability indicator implemented in Surge's
  voltage-stress assessment.

## Transfer Capability

- **[NERC_MOD029]** NERC Standard MOD-029, "Rated System Path Methodology."
  Defines the ATC calculation methodology using PTDF and LODF factors.
  Surge's `compute_nerc_atc` implements this methodology.

- **[NERC_MOD030]** NERC Standard MOD-030, "Flowgate Methodology."
  Defines the AFC calculation methodology using flowgate-specific
  distribution factors.

## HVDC Modeling

- **[Arrillaga1998]** J. Arrillaga, *High Voltage Direct Current
  Transmission*, 2nd ed., IEE Power and Energy Series, 1998.
  Standard reference for LCC-HVDC converter equations and steady-state
  modeling.

- **[Beerten2012]** J. Beerten, S. Cole, and R. Belmans, "Generalized
  Steady-State VSC MTDC Model for Sequential AC/DC Power Flow Algorithms,"
  *IEEE Transactions on Power Systems*, vol. 27, no. 2, pp. 821-829,
  May 2012.
  VSC-MTDC modeling approach. Surge's block-coupled and hybrid HVDC
  solvers build on this formulation.

## Sparse Linear Algebra

- **[Davis2010]** T. A. Davis and E. Palamadai Natarajan, "Algorithm 907:
  KLU, A Direct Sparse Solver for Circuit Simulation Problems," *ACM
  Transactions on Mathematical Software*, vol. 37, no. 3, Article 36, 2010.
  The KLU sparse LU factorization used by Surge for Jacobian and
  admittance matrix solves.

## Test Case Libraries

- **[Zimmerman2011]** R. D. Zimmerman, C. E. Murillo-Sanchez, and R. J.
  Thomas, "MATPOWER: Steady-State Operations, Planning, and Analysis Tools
  for Power Systems Research and Education," *IEEE Transactions on Power
  Systems*, vol. 26, no. 1, pp. 12-19, Feb. 2011.
  Source for MATPOWER test cases used in Surge's example bundles and
  validation.

- **[Babaeinejadsarookolaee2019]** S. Babaeinejadsarookolaee et al.,
  "The Power Grid Library for Benchmarking AC Optimal Power Flow Algorithms,"
  arXiv:1908.02788, 2019.
  Source for PGLib-OPF benchmark cases used in Surge's large-case examples.

## Textbooks

- **[Glover2017]** J. D. Glover, T. J. Overbye, and M. S. Sarma, *Power
  Systems Analysis and Design*, 6th ed., Cengage Learning, 2017.

- **[Grainger1994]** J. J. Grainger and W. D. Stevenson, *Power Systems
  Analysis*, McGraw-Hill, 1994.

- **[Kundur1994]** P. Kundur, *Power System Stability and Control*,
  McGraw-Hill, 1994.
