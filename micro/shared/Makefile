include modules.mk

MODULES := $(foreach model,$(STANDALONE_MODELS),../$(MODEL_DIR_$(model)))

.PHONY: all run clean

all run clean:
	@set -e; for module in $(MODULES); do $(MAKE) -C "$$module" $@; done
