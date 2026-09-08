"""CS336 Assignment 3 (scaling): IsoFLOPs scaling laws.

Implemented following the study cards in notes/cs336-a3/.
"""

from .isoflops import fit_isoflops_scaling_laws, fit_power_law, predict_power_law

__all__ = ["fit_isoflops_scaling_laws", "fit_power_law", "predict_power_law"]
